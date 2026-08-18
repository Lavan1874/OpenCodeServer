# OpenCodeServer v1 acceptance

Builds 1–26 below are historical evidence for their exact source and installed
artifacts. Build 23's query guardian is not part of the current source: ADR 0015
accepts deliberate post-snapshot escape as outside the v1 trust boundary and
replaces the guardian/pending-closeout design with a small, isolated query and
identity-anomaly circuit breaker. That accepted residual risk is not an open
release gate.

The remaining unchecked items are real installed-machine, privacy, TCC/FDA,
FileProviderDomain, accessibility, login/reboot, and UI observations. Checked
historical entries do not close those manual gates.

The following platform gates require an installed, stably signed bundle and
human-observable macOS state.

## Current acceptance status (Build 73, 2026-08-14)

Build 73 is the supervisor-decomposition candidate (ADR 0019): a
behavior-preserving refactor that extracted `CredentialController`,
`VersionQueryCoordinator`, and a stateless `ReattachmentPolicy` from
`Supervisor` (43 → 35 fields; `try_reattach` 144 → 89 lines). It passed every
current-source automated gate locally — `cargo fmt`, `cargo clippy --all-targets
--features test-fixture -- -D warnings`, and the full `cargo test --all
--features test-fixture` suite (125 unit + 65 integration) in both debug and
release, plus a seven-run flakiness sweep of the threaded worker paths. The full
`xcodebuild test` runs in CI on the PR; the Release build (Swift + Rust, stable
`OpenCodeServer Local Signing`) compiled and installed.

The guarded installed update (Build 72 → 73) completed through the transactional
installer; Service Management refreshed OpenCodeServerAgent from the new bundle
(`bundle_version: "73"`), authenticated protocol-5 IPC is reachable, FDA is
`verified`, and the configured endpoint (`10.0.0.254:4096`) and OpenCode version
(`1.18.18`) report correctly. The 72 → 73 bump produced the expected
`access_pending` window (ADR 0016: the XARA partition grant is wiped on a cdHash
change); one interactive "Allow Keychain Access…" click restored it and the
agent converged to `server_state: healthy`, `password_state: configured`,
`config_pending: false`. During that window the menu reported exactly the reason
string the refactored `ReattachmentPolicy::decide_initial` gate-3
`AttachStaleConfig` arm emits for `AccessPending`, confirming the new decision
code runs in production.

The Build 73 evidence above predates the protocol-6 action-capability status
schema. A candidate carrying that current-only schema must repeat the
installed protocol and menu-action acceptance checks; the historical
protocol-5 observations remain evidence for Build 73 only.

The open manual-gate inventory below is unchanged from Build 66: the refactor
touches supervisor Rust only and no privacy, TCC, FDA, FileProviderDomain, Local
Network, or accessibility surface, so the Build 66 evidence for those gates
remains the latest. The automated gates were revalidated for Build 73.
Two 2026-08-16 closures are folded into the inventory from the ADR 0018
amendment / ADR 0021 post-implementation evidence: the conditional Local
Network signing-model gate and the signing-identity lifecycle documentation
item; no other Build 66-era gate was re-run.

Unchecked items below remain real acceptance work unless their indented note
explicitly says that only one branch was observed. They are grouped by the
environment needed to close them:

- **Clean user or restorable VM:** Local Network/mDNS behavior and the
  signing/architecture prerequisites (the measured UI attribution limitation
  is recorded below), pre/post FDA, TCC `AttributionChain`, and File
  Provider/Homebrew/build-upgrade persistence.
- **Installed Build 66 interaction:** the remaining Keychain denial/no-op/
  prompt-cancellation and restart-edge checks, Service Management
  disable/re-enable and combined quit,
  process-control edge cases, menu state combinations, and IPv6 behavior.
- **Human-interface tooling:** Main Thread Checker/Instruments, VoiceOver,
  keyboard editing/navigation, and increased-contrast inspection.

Current open-gate inventory (granular checks, not distinct product defects):

| Section | Open checks |
| --- | ---: |
| Service Management | 7 |
| Keychain credential storage | 19 |
| Local Network privacy and mDNS | 2 |
| Process and network behavior, including IPv6 | 10 |
| Menu layout and progressive disclosure | 5 |
| Accessibility and menu behavior | 12 |
| FDA and responsible code | 5 |
| File Provider and upgrade persistence | 6 |
| **Total** | **66** |

There is no currently reproduced Build 66 code defect in the completed gates.
The Local Network UI fallback was reproduced by an Agent-only diagnostic
operation in Build 66 and recorded as a self-signed/no-Team-ID platform
limitation after its structural prerequisites passed; the 2026-08-16
dual-group clean-state re-test (ADR 0018 amendment, ADR 0021
post-implementation measurement) observed the identical `OpenCodeServerAgent`
attribution under the Apple Development identity, confirming a
signing-model-independent platform limitation that is not an active
bundle-structure repair target. The OpenCodeServerAgent-unreachable menu still
needs one installed Build 66 retest of its corrected `Unable to Determine`
values. The obsolete wording that called the latter a current open code defect
has been removed below.

## Current-source automated gates (Build 73; historical evidence dates retained)

The gate suite was originally recorded against Build 64 on 2026-08-10, was
revalidated for the Build 66 candidate on 2026-08-12, and was revalidated for
the Build 73 candidate on 2026-08-14. The historical evidence
dates below are retained for traceability. These checks do not
replace the unchecked privacy,
accessibility, login/reboot, or UI observations below.

- [x] `CURRENT_PROJECT_VERSION = 66` in both Debug and Release settings
- [x] clean macOS 26/arm64 Debug build and hosted XCTest: 58/58 passed
- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [x] feature-enabled Rust suite: 69 unit tests and 55 process-supervision
      integration tests passed
- [x] Rust integration coverage includes credential-grant cold start,
      foreign-build marker rejection, `access_pending` start refusal,
      username grant isolation, exact protocol-v5 rejection, protocol-v5
      pushed subscriptions, and an external-SIGTERM failure/recovery incident
      whose two persisted notification events have distinct UUIDv4 identities
- [x] Rust unit coverage validates UUIDv4 shape, version and variant bits,
      runtime-state schema 2 round trips, and rejection of schema 1 without a
      compatibility or migration branch
- [x] Swift coverage decodes protocol-5 `event_id`, suppresses persisted and
      in-flight duplicates without ordering IDs, retries requests rejected by
      `UNUserNotificationCenter`, and bounds the accepted-ID ledger to 64
      entries across OpenCodeServer relaunches
- [x] Build 66 configuration serialization contains no password field; the
      password is read from the login Keychain and merged only in memory
- [x] a deterministic race test holds an interactive Keychain read pending,
      changes the configured username, then proves the old account's result is
      discarded without replacing the current account's in-memory credential
      or creating a grant marker
- [x] the Settings form keeps one stable control leading edge across normal,
      checking, interactive-read, edit, removal, and error states; its labels
      are trailing-aligned on a shared native fitting-size column, Advanced
      uses the same column, Port remains compact, and the Startup checkboxes
      do not repeat component-name labels; text-field and window widths are
      derived from native cell-backed `sizeThatFits(_:)` measurements for real
      semantic states, not fixed or zero-width screenshot-tuned constraints
- [x] while Edit or Copy waits for Keychain, only the Password row's small
      activity indicator changes; canceling the system dialog restores
      `Stored in Keychain` without a red error or raw OSStatus
- [x] wrapped runtime feedback refits only the window height; the feedback,
      Save button, Port maximum value, full IPv6 input, executable placeholder,
      and every visible product-owned AppKit control stay unclipped and
      unambiguous
- [x] Build 66 Release, Analyze, Archive, stable-signature, bundle-validation,
      mutual Designated Requirement, and install/rollback fault-injection gates
      passed
- [x] Build 63 → Build 64 installed update completed through the guarded
      workflow: OpenCodeServer exited normally, OpenCodeServerAgent remained
      running until Service Management replaced it, the candidate was atomically
      installed with no staging directory left behind, and the original OpenCode
      PID `11340` was strictly reattached instead of duplicated
- [x] the one-shot development-state transition retained PID `11340`, changed
      runtime-state schema 1 to schema 2 atomically, removed the numeric
      counter, and discarded the stale Build 63 numeric event; no migration or
      compatibility path was added to product code
- [x] authenticated installed OpenCodeServerAgent IPC reports protocol 5 and
      bundle version 64; `RegisteredBundleVersion` committed to 64 only after
      that verification, with endpoint `10.0.0.254:4096` preserved

Immediately after installation, the credential intentionally landed in
`access_pending`, FDA remained `verified`, and OpenCode PID `11340` stayed
supervised with its previous authenticated configuration. The user then chose
“Allow Keychain Access…” for Build 64 and OpenCode returned to authenticated
`healthy`. Later installed incident tests replaced it only through the expected
recovery paths; after the exhaustion test a normal Start restored one healthy
OpenCode on `10.0.0.254:4096`.

## Historical automated gates (through Build 26)

- [x] Xcode 26.6 loads `OpenCodeServer.xcodeproj` and lists the
      `OpenCodeServer` Application Target, `OpenCodeServerTests` Target, and
      shared `OpenCodeServer` Scheme
- [x] clean Xcode Debug build succeeds for macOS 26/arm64
- [x] clean Xcode Release build succeeds for macOS 26/arm64
- [x] Xcode Analyze action succeeds
- [x] Xcode Release Archive succeeds and its archived app passes bundle,
      architecture, Hardened Runtime, App Sandbox, and signature validation
- [x] hosted Xcode XCTest suite passes: 43 tests, including the standard main
      menu, Responder Chain targets, Window menu scope, all editable Settings
      field types, the Service Management update state machine with
      build-identity-gated commit, and the subscription lifecycle state
      machine with the 64 KiB newline-inclusive framing bound
- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [x] Rust feature-enabled tests pass for the current source, including
      installed-version normal/failure/deadline/overflow/descendant cases,
      observed group escape, automatic retry circuit breaking, single-flight,
      orderly query shutdown, durable unverified-state convergence, and two
      independent OpenCodeServerAgent restart lifecycles
- [x] Rust integration tests cover a real OpenCodeServerAgent process
      crash/restart while authenticated fixture OpenCode survives and is
      reattached with the same PID; the separate group-escape restart tests
      also cover Running and Stopped convergence with two independent
      OpenCodeServerAgent processes
- [x] Rust tests cover stale PID cleanup before configuration comparison,
      required semantic fingerprints, and process-inspection uncertainty
- [x] Rust tests cover the post-spawn ownership window, the documented
      installed-version resource lifecycle, durable unverified-state
      convergence, two independent `OpenCodeServerAgent` processes, and
      event-synchronized behavior. ADR 0015 defines the accepted trust boundary;
      harness cleanup is never substituted for product cleanup evidence
- [x] `scripts/test_install.sh` fault injection passes: first install with no
      previous version, normal upgrade keeping no backup, Designated Requirement
      incompatibility leaving the previous bundle unmoved, staging-copy and
      staging-validation failures cleaning staging safely, final-move and
      final-validation failures restoring the previous bundle, a post-install
      restore failure preserving both the previous bundle and the rejected
      candidate in staging, HUP/INT/TERM during the previous-held phase
      restoring the previous bundle with exit codes 129/130/143, a signal in
      the narrow window between the irreversible move and the in-memory phase
      update still restoring the previous bundle, and cleanup refusing to
      delete a staging directory whose identity changed or was replaced with a
      symlink. Signal tests are timeout-protected and complete in under 6
      seconds.
- [x] Swift state/config/Unix-socket and AppKit baseline tests
- [x] Swift tests cover first OpenCodeServerAgent registration, same-version
      idempotence, arbitrarily long cold-start IPC unavailability,
      bundle-version update, asynchronous unregister completion and delayed
      status transition, persistent pending IPC verification, bounded
      changed-bundle retry exhaustion and cross-launch retry-budget
      preservation, later-launch retry after registration rejection, explicit
      repair, and absence of direct OpenCode process signals
- [x] release bundle builds and `scripts/validate_bundle.sh` passes, including
      arm64 and non-empty/unique `LC_UUID` checks for the three bundled native
      binaries
- [x] Xcode generates `Info.plist`, `PkgInfo`, compiled Main storyboard, and
      the asset-catalog app icon
- [x] Release bundle has the required LaunchAgent and three-executable layout
- [x] all three binaries are arm64 Mach-O
- [x] nested OpenCodeServerAgent and opencodeserverctl signatures are created
      before Xcode’s outer app
      CodeSign phase
- [x] strict deep signature validation passes
- [x] Hardened Runtime flags are present on OpenCodeServer,
      OpenCodeServerAgent, and opencodeserverctl
- [x] App Sandbox entitlement is absent from OpenCodeServer,
      OpenCodeServerAgent, and opencodeserverctl
- [x] no credential appears in process arguments, IPC JSON, log messages, or
      repository artifacts

The initial structural checks passed with Xcode 26.6 on 2026-07-29. That
generated Release bundle used an ad hoc signature with Xcode’s CodeSign phase
explicitly applying the runtime option, so that run established structure,
sealed resources, and Hardened Runtime flags only. The updated gates above were
rerun with stable identities on 2026-07-30 as recorded below.

On 2026-07-30, Builds 3 and 4 were also clean-built with
`OpenCodeServer Local Signing`. Both passed strict Bundle/signature validation,
and a real Build 3 → Build 4 installed update completed through
`SMAppService` while preserving the healthy OpenCode PID and listener.

On 2026-07-30, Build 7 was clean-built and archived with
`OpenCodeServer Local Signing` after 29 hosted XCTest cases, Rust formatting,
clippy, 25 Rust unit tests, and 7 process-supervision integration tests passed.
Both the Release product and archive product passed strict Bundle/signature
validation. Build 6 → Build 7 installation preserved OpenCode PID 979 and
`10.0.0.254:4096`; the first bounded registration attempt started
OpenCodeServerAgent PID 36436, strictly reattached OpenCode, verified
authenticated IPC, and committed `RegisteredBundleVersion = 7`. A subsequent
same-version OpenCodeServer quit/reopen kept OpenCodeServerAgent PID 36436,
OpenCode PID 979, and launchd `runs = 1`, with no Service Management mutation.

On 2026-07-31, Builds 8, 9, and 10 were clean-built with
`OpenCodeServer Local Signing` after 33 hosted XCTest cases, Rust formatting,
clippy, 37 Rust unit tests, and 9 process-supervision integration tests
passed (event-driven agent runtime and IPC subscription push, ADR 0009/0010).
Build 8 installed validation exposed a Darwin accept race
(`O_NONBLOCK` inheritance on accepted sockets) that broke IPC handshakes and
blocked the registration transaction; Build 9 installed validation exposed a
busy-spin beginning at the 300 s `STABLE_RUN_INTERVAL` mark. Both defects
were fixed with regression tests (ADR 0009 addenda). Build 10 then installed
atomically over Build 9: the bounded transaction recovered from one stale
launch-constraint spawn, OpenCodeServerAgent reattached OpenCode PID 4876
through strict identity and health checks, authenticated IPC verified, and
`RegisteredBundleVersion = 10` committed. The agent held 0.0% CPU past the
300 s mark, `10.0.0.254:4096` and FDA `Verified` were preserved throughout,
and no SIGPIPE, IPC failure, or unregister/register loop appeared in
Unified Logging.

On 2026-08-01, Builds 14, 15, and 16 landed as targeted fixes recorded in
git history: Build 14 runs OpenCodeServerAgent as `ProcessType =
Interactive` (settled product decision, ADR 0012, guarded by
`scripts/validate_bundle.sh`); Build 15 waits for the endpoint address
during startup; Build 16 keeps the supervised process identity when the
Homebrew executable file is replaced underneath it. Build 16 was the
installed production build immediately before the Build 17 upgrade: it
reported healthy over authenticated IPC with OpenCode PID 644 listening
only on `10.0.0.254:4096`, FDA `Verified`, OpenCode 1.18.10, and
`RegisteredBundleVersion = 16`.

On 2026-08-01, Build 17 (the Build 16 rework directive) was clean-built
with `OpenCodeServer Local Signing` after a clean hosted XCTest run of 43
tests (23 model/state, 15 Service Management, 5 AppKit baseline), Rust
formatting, clippy, 48 Rust unit tests, and 27 process-supervision
integration tests passed. The signed Release product and the Release
archive both passed strict bundle/signature validation: all three
executables and the outer app carry Hardened Runtime, are arm64-only, and
hold no App Sandbox entitlement. Designated Requirements are mutually
compatible in both directions between the installed Build 16 and both
Build 17 products. An isolated run of the Build 17 OpenCodeServerAgent
(own support directory, desired state stopped) reported
`bundle_version = "17"` over IPC without spawning OpenCode.

Build 17 then installed over Build 16 on this Mac through the standard
workflow (graceful OpenCodeServer quit, `scripts/install.sh`, reopen). The
bounded update transaction needed two attempts: attempt 1 returned
registration success but could not be verified over authenticated IPC
within its verification window (the documented macOS 26 stale-launch
behavior; the window was 30 seconds at the time and is adaptive since the
ADR 0006 addendum of 2026-08-03); bounded attempt 2 launched OpenCodeServerAgent PID 18747, which
proved its build identity (`bundle_version = "17"`) at 21:38:22 local,
committing `RegisteredBundleVersion = 17`. OpenCode PID 644 (started
20:12:55, `/opt/homebrew/bin/opencode serve`) was never signaled: the new
OpenCodeServerAgent reattached it through strict identity and
authenticated health checks, and it remained the only listener on
`10.0.0.254:4096` with FDA `Verified` and OpenCode 1.18.10 throughout. The
Unified Logging scan of the window shows no credential material, signature
rejection, launch-constraint violation, or crash — only the expected
error-level note that attempt 1 did not verify and bounded attempt 2 was
scheduled.

On 2026-08-02, Build 18 (the Kimi rework directive) was clean-built with
`OpenCodeServer Local Signing` after 43 hosted XCTest cases, Rust formatting,
clippy, 48 Rust unit tests, and 30 process-supervision integration tests
passed. The signed Release product and the Release archive both passed strict
bundle/signature validation; all three executables and the outer app are
arm64-only with Hardened Runtime and no App Sandbox. Designated Requirements
are mutually compatible in both directions with the installed Build 17.

Build 18 installed over Build 17 through the standard workflow (graceful
OpenCodeServer quit, `scripts/install.sh`, reopen). Before the upgrade:
OpenCodeServer PID 485, OpenCodeServerAgent PID 580, OpenCode PID 676
(`opencode serve --hostname 10.0.0.254 --port 4096`),
`RegisteredBundleVersion = 17`. After the upgrade: OpenCodeServerAgent PID
74288 (new), OpenCode PID 676 (unchanged, never signaled),
`RegisteredBundleVersion = 18` (committed after authenticated IPC),
`10.0.0.254:4096` preserved, FDA `Verified`, OpenCode 1.18.10. No staging
directory or historical backup was left behind. (The later OpenCode PID
change to 75671 was the product owner's manual restart, not an upgrade
defect.)

On 2026-08-02, Build 19 (the GLM rework directive) was clean-built with
`OpenCodeServer Local Signing` after Rust formatting, clippy
`-D warnings`, 48 Rust unit tests, and 39 process-supervision integration
tests passed, including 10 consecutive full-suite rounds under default
10-way parallelism and 3 isolated runs of the previously flaky
`version_queries_are_single_flight_and_every_hung_child_is_reaped`
regression test. `scripts/test_install.sh` (Build 18 → Build 19) exits 0
with all fault-injection scenarios passing. A Debug clean build and the
hosted XCTest suite (43 tests, Apple Development identity
<apple-development-sha1>) passed, Xcode Analyze
succeeded, and a Release Archive was produced. The Release product and the
archive both passed strict Bundle/signature validation: all three
executables and the outer app are arm64-only with Hardened Runtime and no
App Sandbox entitlement, and Designated Requirements are mutually
compatible in both directions with the installed Build 18.

Build 19 installed over Build 18 through the standard workflow (graceful
OpenCodeServer quit, `scripts/install.sh`, reopen). Before the upgrade:
OpenCodeServer PID 74143, OpenCodeServerAgent PID 74288, OpenCode PID
75671 (`opencode serve --hostname 10.0.0.254 --port 4096`),
`RegisteredBundleVersion = 18`, endpoint `10.0.0.254:4096`, FDA
`Verified`. After the upgrade: OpenCodeServer PID 37638 (new),
OpenCodeServerAgent PID 37779 (new; launchd `runs = 1`, state running),
OpenCode PID 75671 (unchanged, never signaled — its uptime counter
continued without a gap), `RegisteredBundleVersion = 19` (committed only
after authenticated IPC reported `bundle_version = "19"` from the new
OpenCodeServerAgent), `10.0.0.254:4096` preserved, FDA `Verified`,
OpenCode 1.18.10 healthy. The Unified Logging scan of the upgrade window
shows the old OpenCodeServerAgent's graceful exit ("exiting without
signaling the managed OpenCode"), the new OpenCodeServerAgent's strict
reattachment ("Strict process identity and health checks passed;
reattached to OpenCode"), and no Launch Constraint Violation, signature
rejection, crash, or credential material. No staging directory or
historical backup was left behind.

Build 20 status: commit `405d87d` changed the project version from 19 to 20
and the prior review recorded an installed Build 20 runtime snapshot. That is
not Build 20 Release/Analyze/Archive evidence. The review also found that the
installed-version query, durable `Missing` transition, two-process escape
coverage, and parallel-test causality still had blocking gaps. Those reports
remain useful historical observations only and are superseded by
ADR 0015 and the Build 21 Phase 7/8 evidence matrix (the interim
`REWORK_DIRECTIVE.md` was removed with the Build 24-era tracking files; its
content survives in git history and ADR 0015).

Build 21 Phase 0–5 source-tree evidence is now recorded in commits
`947b4cd` through `3040fba`. Phase 2–4 targeted tests cover the query
ownership boundary, durable unverified transitions, and two independent
`OpenCodeServerAgent` restart lifecycles. Phase 5 replaces elapsed-time branch
proof with per-fixture event traces and a held-listener bind handoff. Its
10-round default-parallel, 20-round exact, and bounded CPU/IO-load results are
source-tree evidence only. Phase 6 platform/documentation review is recorded in
commit `fbd9af5`.

On 2026-08-02, the Build 21 Phase 7 candidate gates passed after the project
version was raised to 21: clean Debug and Release builds, 53 unit plus 48
integration Rust tests, hosted XCTest (43 tests), Analyze, Release Archive,
strict bundle/signature validation, and the guarded install-workflow test
suite. The Release product and archive use `OpenCodeServer Local Signing`, all
three executables are arm64 with Hardened Runtime and no App Sandbox, and the
candidate Designated Requirements are mutually compatible with the installed
Build 20 bundle. The Phase 7 delivery record is commit `f9baf2c`. The
installed-app cutover record and the remaining privacy/manual acceptance are
tracked in Phase 8 below.

On 2026-08-02, the guarded installed Build 20 → Build 21 upgrade completed.
Before installation, OpenCodeServer PID `64986`, OpenCodeServerAgent PID
`65121`, and OpenCode PID `75671` were recorded; authenticated status reported
OpenCodeServerAgent bundle version `20`, `RegisteredBundleVersion = 20`,
healthy OpenCode, FDA `Verified`, endpoint `10.0.0.254:4096`, and OpenCode
start time `2026-08-02 03:17:32 +0800`. OpenCodeServer received a normal quit
request and the workflow issued no stop or signal action for OpenCodeServerAgent
or OpenCode. `scripts/install.sh` completed the atomic replacement and left no
staging directory or backup. After relaunch, OpenCodeServer PID `28748` and
OpenCodeServerAgent PID `28861` were running; authenticated
`opencodeserverctl status --json` reported bundle version `21`, healthy
OpenCode, FDA `Verified`, the same endpoint, and `RegisteredBundleVersion = 21`.
OpenCode remained PID `75671` with the same start time, exactly one `opencode`
process was present, and launchd reported the Build 21 OpenCodeServerAgent job
running at PID `28861`. A non-printing Unified Logging scan of the upgrade
window sampled 554 records and found zero matches for launch-constraint,
signature, crash, or credential terms. The Phase 8 delivery record is commit
`8435d3f`.

## Post-Phase 8 completion audit

The source audit in commits `cf7e07e`, `96946e7`, `c834ae6`, and `9c8af07` found and
corrected a remaining installed-version cleanup risk: the bounded query path no
longer falls back to an unbounded `Child::wait()`, and force cleanup no longer
treats a failed current identity snapshot as authorization for direct signaling.
It hands overdue cleanup to the supervisor-owned pending state while bounded
nonblocking reap closeout continues, gates the test-only query hook, and
requires authorized group membership before group signaling. The query worker
now has an owned `JoinHandle`; normal OpenCodeServerAgent shutdown drains the
supervisor-owned query state and joins the worker before dropping its receiver,
so a normal shutdown does not detach an owned query Child.

The corrected Build 22 candidate (version bump commit `a46c236`, containing
source commit `9c8af07`) passed the Rust 55-unit/49-integration suite, targeted
query tests, 10 default-parallel full-suite rounds, 20 exact rounds each for
overflow, inherited-stdout descendant, and single-flight tests, and the
10-test installed-version event suite under bounded temporary CPU/IO load.
It also passed clean Debug/Release builds, hosted 43-test XCTest,
Analyze, Archive, strict bundle/signature validation, and the guarded
install-workflow test. The Release product and archive use `OpenCodeServer Local
Signing`; all three executables are arm64 with Hardened Runtime and no App
Sandbox, and the candidate Designated Requirements are mutually compatible
with the installed Build 21 bundle. The validated candidate and archive are
`build/OpenCodeServer.app` and
`build/Archives/Build22/OpenCodeServer.xcarchive`.

On 2026-08-02, the corrected Build 21 → Build 22 upgrade completed. Before
installation, OpenCodeServer PID `28748`, OpenCodeServerAgent PID `28861`, and
OpenCode PID `75671` were recorded; authenticated status reported bundle
version `21`, `RegisteredBundleVersion = 21`, healthy OpenCode, FDA `Verified`,
endpoint `10.0.0.254:4096`, and OpenCode start time
`2026-08-02 03:17:32 +0800`. OpenCodeServer received the standard normal quit
request and exited within the bounded wait. The workflow issued no stop or
signal action for OpenCodeServerAgent or OpenCode. `scripts/install.sh`
completed the atomic replacement and left no staging directory or backup.

After relaunch, OpenCodeServer PID `81818` and OpenCodeServerAgent PID `82006`
were running from the Build 22 bundle; launchd reported the job running with
parent bundle version `22`. The registration transaction needed its bounded
second attempt: the first authenticated-IPC wait was unverified, and the
second attempt was verified at `2026-08-02 13:50:01 +0800`. Only then did
OpenCodeServer persist `RegisteredBundleVersion = 22`. Authenticated
`opencodeserverctl status --json` then reported bundle version `22`, healthy
OpenCode, FDA `Verified`, the same endpoint, and OpenCode `1.18.10`. The
configured Homebrew OpenCode image remained the single process at PID `75671`,
with the same start time; no second configured OpenCode image was present.
The installed native executables exactly matched the validated candidate by
SHA-1. A non-printing Unified Logging scan sampled 836 records and found zero
matches for the required launch-constraint, signature-rejection, crash,
spawn-failure, or credential-leak patterns. The benign process-manager
`SIGNAL` records and the explicit OpenCodeServerAgent message saying it exited
without signaling OpenCode were not product signal actions.

The newer Build 22 installation verifies the post-Phase 8 source-to-installed
boundary. Build 23 then added a short-lived native query guardian using the
signed OpenCodeServerAgent executable itself. The isolated real-process test
`killed_opencodeserveragent_query_guardian_closes_the_query_group` proves that
an active dedicated in-group query is gone after OpenCodeServerAgent `SIGKILL`.

On 2026-08-02, the Build 23 candidate (source commit `d4a26ef`, version bump
commit `317e298`) passed the Rust 55-unit/50-integration suite, the targeted
query regressions, ten default-parallel full-suite rounds, twenty exact rounds
for the overflow/inherited-stdout/single-flight regressions, the event suite
under bounded CPU/IO load, clean signed Debug and Release builds, Analyze,
Release Archive, strict bundle/signature/arm64/Hardened Runtime/no-App-Sandbox
validation, mutual Designated Requirement checks against installed Build 22,
and `scripts/test_install.sh`. The hosted 43-test XCTest suite passed with the
exact Apple Development signing identity fingerprint
`<apple-development-sha1>`; its result bundle is
`build/TestResults/Build23-AppleDevelopment-entitled.xcresult` and reports
43/43 passed. The Release product and archive use stable
`OpenCodeServer Local Signing`; the validated candidate and archive are
`build/OpenCodeServer.app` and
`build/Archives/Build23/OpenCodeServer.xcarchive`.

On 2026-08-02, the guarded Build 22 → Build 23 installation completed. Before
installation, OpenCodeServer PID `81818`, OpenCodeServerAgent PID `82006`, and
OpenCode PID `75671` (started `2026-08-02 03:17:32 +0800`) were recorded;
authenticated status reported bundle version `22`, `RegisteredBundleVersion =
22`, healthy OpenCode, FDA `Verified`, endpoint `10.0.0.254:4096`, and
OpenCode 1.18.10. OpenCodeServer received only the standard normal quit
request; no stop or signal action was issued for OpenCodeServerAgent or
OpenCode. `scripts/install.sh` completed the atomic replacement with no
staging directory or backup left behind.

After relaunch, OpenCodeServer PID `10596` and OpenCodeServerAgent PID `10781`
were running from Build 23; launchd reported the job running with parent bundle
identifier `ai.opencode.server`, parent bundle version `23`, and PID `10781`.
Authenticated `opencodeserverctl status --json` reported bundle version `23`,
healthy OpenCode, FDA `Verified`, endpoint `10.0.0.254:4096`, OpenCode 1.18.10,
and OpenCode PID `75671` with the same start time. `RegisteredBundleVersion =
23` was persisted only after authenticated IPC proved the new
OpenCodeServerAgent; the installed-version query then reported 1.18.10. The
three installed native executable hashes exactly matched the validated
candidate. A non-printing Unified Logging scan sampled 469 records and found
zero matches for launch-constraint, signature, crash, spawn-failure, or
credential-leak patterns. The remaining privacy, TCC, FileProviderDomain,
accessibility, reboot, and other manual acceptance items remain pending.

An uninterruptible direct query child can still keep an orderly
OpenCodeServerAgent shutdown pending. A deliberately reparented group-escape
descendant is an accepted v1 residual risk and non-goal, not an unverified
containment claim; no foreign group or inferred descendant PID may be signaled.

On 2026-08-02, the Build 24 candidate (source commit `45b95a9`, version bump
commit `20f2619`) added fail-closed observation of a query leader that escapes
its authorized process group while still observable. The new regression
`installed_version_query_rejects_a_group_escape_without_signaling_the_foreign_group`
passed in the full 55-unit/51-integration feature suite. The candidate also
passed ten default-parallel full-suite rounds, twenty exact rounds each for
overflow, inherited-stdout descendant, and single-flight, and the ten-test
event-causality suite under bounded CPU/IO load. Rust formatting, check,
clippy, release build, and the release binary no-fixture-symbol check passed.

Build 24 then passed clean signed Debug and Release builds, hosted XCTest
(43/43 with Apple Development fingerprint
`<apple-development-sha1>`), Analyze, Release Archive, both
Release/Archive bundle validations, strict signatures, arm64,
Hardened Runtime, no App Sandbox, mutual Designated Requirements, and
`scripts/test_install.sh`. The saved XCTest result is
`build/TestResults/Build24-AppleDevelopment-entitled.xcresult`; the Release
product and archive are `build/OpenCodeServer.app` and
`build/Archives/Build24/OpenCodeServer.xcarchive`.

The guarded Build 23 → Build 24 installation preserved OpenCode PID `75671`
and start time `2026-08-02 03:17:32 +0800`. Before cutover, OpenCodeServer PID
`18792`, OpenCodeServerAgent PID `10781`, and authenticated status bundle
version `23` were recorded; health was `healthy`, FDA was `Verified`, the
endpoint was `10.0.0.254:4096`, and installed/running OpenCode was `1.18.10`.
OpenCodeServer received only the normal quit request. After relaunch,
OpenCodeServer PID `36833` and OpenCodeServerAgent PID `36992` were running;
OpenCode remained PID `75671` with the same start time and listener. Launchd
reported parent bundle identifier `ai.opencode.server`, parent bundle version
`24`, and a running job. Authenticated IPC reported bundle version `24`, and
only then did `RegisteredBundleVersion = 24` persist. It reported healthy
OpenCode, FDA `Verified`, the same endpoint, and OpenCode `1.18.10`.

Installed and candidate nested executable hashes matched exactly:
OpenCodeServerAgent `1f1705bdf8b5cb29fc387ab60718ef6d8d282af3462765300660622d0fc1c041`
and opencodeserverctl
`0befb44b6c5005258e41d495ae2c65353dae287ae5b753dcca215efc13cb1877`.
The listener check showed only OpenCode PID `75671` on `10.0.0.254:4096`;
the Applications directory had no staging or backup remnants. The bounded
upgrade log recorded the old OpenCodeServerAgent exiting without signaling
the managed OpenCode, authenticated reattachment by the new agent, and IPC
verification for bundle version 24. A time-bounded scan found zero prohibited
OpenCode-signal log records. The first registration attempt timed out on
authenticated IPC and the persisted bounded transaction retried once; the
second attempt succeeded without stopping OpenCode.

Build 24 improved the observed-live group-escape case. The later product
decision in ADR 0015 accepts the remaining post-snapshot escape race and
uninterruptible-child finite-shutdown limitation as residual risks. Manual
privacy, TCC, FileProviderDomain, accessibility, reboot, and related acceptance
work remains open.

- [x] install and authenticate Build 22 containing post-Phase 8 audit commits
      `cf7e07e`, `96946e7`, `c834ae6`, and `9c8af07`; authenticated IPC proved
      the new agent before `RegisteredBundleVersion = 22` was committed
- [x] install and authenticate Build 23 containing the native query guardian;
      authenticated IPC proved the new agent before `RegisteredBundleVersion =
      23` was committed, and the configured OpenCode PID/start time were
      preserved
- [x] install and authenticate Build 24 containing observed group-escape
      fail-closed hardening; authenticated IPC proved the new agent before
      `RegisteredBundleVersion = 24` was committed, and the configured
      OpenCode PID/start time were preserved

## Build 25–26 simplification evidence

- [x] installed-version logic is isolated in `rust/src/version_query.rs`; the
      main supervisor no longer owns guardian or pending-cleanup states
- [x] observed query group escape/identity anomaly kills only the owned direct
      child, never the foreign group, and suppresses automatic retries for the
      same executable until path change or OpenCodeServerAgent restart
- [x] current source removes the query guardian mode and
      `proc_listpgrppids` process-group enumeration
- [x] post-snapshot deliberate escape is documented in
      `PRODUCT_DECISIONS.md`, `AGENTS.md`, plan, and ADR 0015 as an accepted v1
      trust-boundary non-goal
- [x] current candidate passes clean Debug/Release, hosted XCTest, Analyze,
      Archive, bundle/signature/architecture/Hardened Runtime/App Sandbox, and
      install-workflow gates

On 2026-08-02, Build 25 validated the simplified current source. Rust formatting
and clippy passed; the feature-enabled suite passed 52 unit and 51 process-
supervision integration tests, including the automatic query circuit breaker
and its reset after an executable-path change. Clean Debug and Release builds,
Analyze, Release Archive, 43/43 hosted XCTest cases with Apple Development
fingerprint `<apple-development-sha1>`, and strict validation of
both the Release and archived products passed. The Release products use
`OpenCodeServer Local Signing`, remain arm64-only with Hardened Runtime and no
App Sandbox, and have mutually compatible Designated Requirements with the
installed Build 24. The isolated install/rollback fault-injection suite passed.

Build 25 was then installed over Build 24 through the standard workflow.
OpenCode PID `75671`, its `2026-08-02 03:17:32 +0800` start time, and the
`10.0.0.254:4096` listener remained unchanged. The bounded Service Management
transaction recovered on attempt 2 from the known macOS stale launch-
constraint behavior, authenticated OpenCodeServerAgent PID `69375`, and only
then committed `RegisteredBundleVersion = 25`. Status was healthy, FDA remained
`Verified`, and no staging directory or backup remained.

Installed observation then found that a successful `opencode --version` query
still attempted a speculative process-group `SIGKILL` after `NOTE_EXIT` and
stdout EOF. macOS rejected that unnecessary signal with `EPERM`; OpenCode and
health were unaffected, but the error repeated on the 60-second informational
query cadence. Build 26 reserves process-group cleanup for incomplete, invalid,
or timed-out queries and directly reaps a clean result without signaling it.
The exact regression asserts that a normal query emits no signal request.

Build 26 passed Rust formatting and clippy, 52 unit and 51 integration tests,
clean signed Debug and Release builds, Analyze, Release Archive, and 43/43
hosted XCTest cases with Apple Development fingerprint
`<apple-development-sha1>`. Both Release products passed strict
bundle/signature validation and have mutually compatible Designated
Requirements with installed Build 25. The isolated install/rollback fault-
injection suite also passed. Installed-machine and privacy acceptance remain
the explicit manual gates below.

Build 26 was installed over Build 25 through the standard workflow. The
transaction completed on bounded attempt 2; authenticated IPC proved
OpenCodeServerAgent PID `83366` before `RegisteredBundleVersion = 26` was
committed. OpenCode PID `75671`, its start time, and the sole listener on
`10.0.0.254:4096` remained unchanged; status was healthy and FDA remained
`Verified`. The installed bundle passed strict validation, and installation
left no staging directory or backup. Across the initial query and a complete
subsequent 60-second refresh interval, the new OpenCodeServerAgent emitted no
process-group close error, identity-anomaly circuit-breaker event, or group-
escape event. The older `EPERM` records stop with Build 25 PID `69375` before
the Build 26 handoff.

## Service Management

Use `/Applications/OpenCodeServer.app`, not a development build path.

- [ ] Xcode opens the project in its GUI without conversion or missing-file
      warnings
- [x] stable-identity Release build and archive validate successfully; the
      guarded install-workflow test suite passes
- [ ] `/Applications/OpenCodeServer.app` launches without a Dock icon
- [ ] first launch registers OpenCodeServerAgent and OpenCodeServer independently
- [x] OpenCodeServerAgent starts immediately or clearly reports `requiresApproval`
- [ ] System Settings disablement changes `SMAppService.Status`
- [ ] disabled OpenCodeServerAgent is not presented as running
- [ ] re-enable and re-registration work
      - Build 62 installed observation confirmed that disabling Background
        Activity removed the launchd job and IPC socket while leaving the
        verified OpenCode PID running; re-enable/Repair later restored
        OpenCodeServerAgent control without duplicating OpenCode. That run also
        exposed the stale-value wording defect described in the menu section.
        Current source fixes the wording, but the complete disable/re-enable
        sequence has not been repeated on Build 64, so these three gates remain
        open.
- [x] replacing a bundle and re-registering OpenCodeServerAgent preserves the healthy
      OpenCode child through strict reattachment
- [x] guarded installed Build 20 → Build 21 upgrade preserves the OpenCode PID,
      endpoint, health, FDA state, and OpenCode start time; authenticated IPC
      proves Build 21 before `RegisteredBundleVersion = 21` is accepted, and
      the successful install leaves no staging directory or backup
- [x] corrected installed Build 21 → Build 22 upgrade preserves the configured
      OpenCode PID, endpoint, health, FDA state, and OpenCode start time;
      authenticated IPC proves Build 22 before `RegisteredBundleVersion = 22`
      is accepted, and the successful install leaves no staging directory or
      backup
- [x] installed Build 22 → Build 23 upgrade preserves the configured OpenCode
      PID, endpoint, health, FDA state, and OpenCode start time; authenticated
      IPC proves Build 23 before `RegisteredBundleVersion = 23` is accepted,
      and the successful install leaves no staging directory or backup
- [x] installed Build 23 → Build 24 upgrade preserves the configured OpenCode
      PID, endpoint, health, FDA state, and OpenCode start time; authenticated
      IPC proves Build 24 before `RegisteredBundleVersion = 24` is accepted,
      and the successful install leaves no staging directory or backup
- [x] after a changed OpenCodeServerAgent executable is registered, do not
      accept the new `RegisteredBundleVersion` until authenticated IPC proves
      OpenCodeServerAgent
      executed; verify an unverified attempt retries on the next OpenCodeServer
      launch
- [x] with an enabled same-version registration, temporary
      OpenCodeServerAgent IPC unavailability never calls `unregister()` or
      `register()` and automatically recovers on a later poll
- [x] an accepted but not-yet-IPC-verified bundle update persists
      `OpenCodeServerAgentPendingBundleVersion` and its bounded attempt number;
      restarting OpenCodeServer resumes verification and cannot exceed three
      attempts in the same transaction
- [x] inspect BTM/launchd logs during a real Build N → Build N+1 update and
      confirm bounded recovery leaves no stale launch constraint or
      `spawn failed` job
- [x] with OpenCode running, restart macOS and verify the stale pre-reboot PID is
      discarded, a new OpenCode PID starts, and no false “existing process”
      configuration-change failure appears
      - Build 63 installed-machine evidence, 2026-08-10: macOS booted at
        `21:30:25`; OpenCodeServer PID `475` and OpenCodeServerAgent PID `539`
        started independently at `21:30:37` and `21:30:39`. OpenCodeServerAgent
        logged that the stale recorded PID was absent and no process was
        signaled, then started OpenCode PID `653` at `21:30:43`; health became
        `healthy` at `21:30:48` without an existing-process/configuration error.
- [x] verify `.config-fingerprint.key` is a regular user-owned mode `0600` file
      and neither it nor `state.json` contains the configuration password
- [x] Quit OpenCodeServer leaves OpenCodeServerAgent and OpenCode running
- [ ] Stop OpenCode and Quit OpenCodeServer stops OpenCode, unregisters
      OpenCodeServerAgent, and quits OpenCodeServer

## Keychain credential storage

The installed Build 66 was tested on the retained `macOS 3.utm` baseline VM.
The final VM state has no test credential in the login Keychain, no password
field in `config.plist`, and OpenCodeServerAgent is healthy in
`not_configured` state. When inspecting the item ACL manually in Keychain
Access, restart Keychain Access first — its UI does not reliably refresh ACL
changes (TN3137, r.82556933).

### Build 66 baseline-VM evidence (2026-08-12)

The temporary credential matrix was run on `macOS 3.utm` only. The diagnostic
VM was permanently removed before testing. First creation wrote a login-
Keychain item while leaving `config.plist` free of a password; Save itself did
not raise a system prompt. An explicit `Allow Keychain Access…` followed by
the user's `Always Allow` completed the Agent grant, changed the status to
`configured`, sent one SIGTERM to the old OpenCode process, and started one
healthy replacement. A password change with `Later` left the old process
supervised and the credential in `access_pending`; the subsequent explicit
grant and automatic restart converged to healthy. Application removal
produced `credential_removed` behavior: the item was deleted, the Agent
became `not_configured`, and OpenCode restarted once in native unauthenticated
mode. External Keychain deletion and external value editing were exercised
without an Agent crash; dedicated HTTP 401 guidance for an externally changed
value remains a separate open check below. While authorization was pending,
repeated IPC status requests continued to return promptly with the same Agent
PID, OpenCode health, and endpoint, so the background read did not block
supervision. The temporary item was deleted at the end of the run.

Build 62 source note: creating the first password item does not send
`credential_changed` because there is no carried-over old credential to
invalidate. When OpenCode is running, however, `.created` does select the
immediate “Allow & Restart” offer, just like a real update; Save itself remains
non-interactive and only the user's primary-button click requests Keychain
access. When OpenCode is stopped, no restart alert appears and Settings
discloses the `Agent access` row and “Allow Keychain Access…” button instead.

- [x] saving a password in Settings stores it only in the login keychain
      (service `ai.opencode.server`, account = effective username);
      `config.plist` stays free of it and no prompt appears for the GUI
      itself
      - Build 66 `macOS 3.utm` matrix: the item was found with
        `security find-generic-password` while `config.plist` still had no
        password field; Save completed without a system authorization prompt.
- [x] after Save, OpenCodeServerAgent’s background config reload raises NO
      Keychain prompt either (macOS 26 regression lock: routine work uses
      only the attribute-only probe; a background decrypt is attempted
      solely with a recorded grant)
      - Build 63 installed password-update walkthrough: Save produced only the
        product's contextual Allow & Restart choice. The system Keychain dialog
        appeared only after the user chose that action; no config-reload or
        periodic background prompt preceded it.
- [ ] with the item saved but not yet granted, the menu shows
      `Password: Access not granted — open Settings`, the Settings
      `Agent access` row shows `Not granted`, and one reminder notification
      arrives per episode
- [ ] the item created at Save time already lists OpenCodeServerAgent in
      its ACL (Keychain Access → Access Control shows it under “Always
      allow access by these applications”); if pre-seeding failed, creation
      fell back to the default ACL (logged)
- [ ] starting OpenCode while access is pending fails with an actionable
      error shown in a visible alert, and never starts OpenCode without
      authentication
- [x] `Allow Keychain Access…` raises the system authorization prompt in the context
      of the click; choosing “Always Allow” flips the row to `Granted`,
      resumes a pending start, and health checks authenticate
      - Build 66 `macOS 3.utm` matrix: the user chose `Always Allow`; the Agent
        became `configured`, authenticated health was healthy, and the old
        OpenCode PID was replaced exactly once.
- [ ] that single “Always Allow” is complete: restarting
      OpenCodeServerAgent afterwards (`launchctl kickstart -k
      gui/501/ai.opencode.server.agent`) reads the credential with NO
      second prompt (macOS 26 two-stage consent lock, ADR 0016)
- [x] the Settings `Agent access` row and the menu update live as agent
      status pushes arrive — no Settings window reopen needed
      - During the installed password-change authorization, the open Settings
        window converged from pending access back to `Stored in Keychain` and
        `Granted` without being reopened; the automatic restart completed
        without a second confirmation alert.
- [x] after a grant, restarting OpenCodeServerAgent or the Mac re-reads the
      credential silently — no prompt (persisted grant marker)
      - During the Build 63 reboot above, the bounded worker changed
        `AccessPending` to `Available` in the same `21:30:40.415` log instant,
        authenticated health subsequently passed, and the user confirmed that
        no background Keychain authorization dialog appeared.
- [ ] choosing “Don’t Allow” or cancelling leaves the soft
      `access_pending` state: no deletion, no restart loop, no second
      prompt from background reads
- [ ] saving an UNCHANGED password again is a no-op: no `SecItemUpdate`
      happens, the item `mdat` does not move, and a later agent restart
      still reads silently (macOS 26 regression lock: any update wipes the
      XARA partition list, ADR 0016)
- [x] changing the password updates the item in place (no delete+add);
      the change revokes OpenCodeServerAgent's grant on macOS 26, so Save
      raises NO prompt itself: the `Agent access` row flips to
      `Not granted` (via the non-interactive `credential_changed` notice),
      one “Allow Keychain Access…” click raises the consent dialog in the context of
      that click — never a background prompt — and the agent then holds the
      NEW password
      - `KeychainStore.update` uses `SecItemUpdate` for the changed-existing
        branch and the hosted regression covers it. The Build 63 installed
        walkthrough observed the expected pending grant, one explicit consent,
        and no second restart alert; the original password was restored through
        the same flow after the test.
- [ ] regression lock (v47 bug): after a password change and re-authorization,
      “Restart OpenCode…” launches OpenCode with the NEW password — health
      checks must authenticate with the new value, not the carried-over old
      one
- [ ] saving with changed settings while OpenCode is running offers
      “Restart OpenCode to apply the changes?” with【Restart OpenCode】and
      【Later】; choosing restart converges to green, choosing Later leaves
      the stale process managed with `Configuration: Restart pending`
      (yellow, not the old unrecoverable red dead end, ADR 0005 2026-08-05
      amendment)
- [ ] dead-end regression lock: change the password, choose【Later】, then
      “Repair OpenCodeServerAgent…” — the agent takes over the
      stale-configuration process (stoppable/restartable, `Restart pending`),
      never abandons it as unverified; “Restart OpenCode…” then converges to
      green without any manual process killing
- [x] opening Settings, including after an upgrade where OpenCodeServer has no
      current Keychain grant, performs only an attribute-only background probe,
      displays `Stored in Keychain`, and raises NO system Keychain dialog;
      Unified Logging shows no decrypt-class `SecItemCopyMatching` from simply
      opening the window
      - After the progressive-disclosure fix was installed, opening Settings
        stayed quiet. The system authorization dialog appeared only after the
        user explicitly chose Edit or Copy, matching the off-main-thread source
        paths and hosted tests.
- [x] `Edit…` and `Copy` are the only GUI actions that decrypt the saved
      password; either may raise the system Keychain dialog in the context of
      that click, while the Settings window, menu bar, IPC monitoring, and
      OpenCodeServerAgent supervision remain responsive
      - Both installed actions were exercised. While the system dialog was
        pending, OpenCodeServerAgent continued supervision and only the fixed
        Password-row activity indicator changed.
- [x] an existing credential is never represented by a prefilled field:
      `Edit…` loads it into a concealed field, `Show` reveals it only after
      that explicit edit action, `Copy` copies only after its explicit action,
      and cancelling Edit performs no Keychain write
      - The user confirmed concealed-by-default Edit, explicit Show and
        re-conceal, correct current-password display, Copy, Cancel Edit
        (formerly "Keep Saved"), and
        cancel-without-write behavior on the installed app.
- [ ] `Remove…` first shows `Will be removed when you save`; `Undo` and Cancel
      preserve the item, and only Save deletes it. An empty edit field is not
      interpreted as deletion
- [x] a Save that updates an existing password while OpenCode is running shows
      ONE dialog immediately: while
      authorization is pending its primary button is “Allow & Restart”;
      clicking it raises the system consent prompt (Save itself never does),
      and once the agent reports `configured` the restart fires automatically —
      no second alert, no orphaned pending-restart state. Choosing【Later】
      leaves the usual `Restart pending` convergence path
      - The installed password-change walkthrough confirmed the primary action,
        one consent dialog, automatic restart, and absence of a second restart
        confirmation.
- [ ] the first-nonempty-password Save while OpenCode is running follows the
      same one-dialog Allow & Restart path; this creation branch still needs a
      clean item because its ACL pre-seeding behavior differs from update
- [ ] that same first-password Save while OpenCode is stopped raises no modal
      alert; Settings reveals the `Agent access` row and
      “Allow Keychain Access…” button, so authorization remains available in
      context without interrupting the user
- [x] the Settings window keeps the width derived from native control metrics
      and anticipated semantic content unchanged when long green/red feedback
      messages wrap; only the window height may grow to fit that feedback
      - Installed screenshots covered normal, Keychain-waiting, error, and
        restored states after the native sizing fix; the control leading edge
        and window width stayed fixed while only vertical content changed.
- [ ] a fully unchanged Save (same settings, same credential, same login-item
      choices) shows the neutral `No changes to save.` — never the green
      "restart to apply" advice — and performs no Keychain update
- [ ] after a changed Save, the "restart to apply" advice retires on its own:
      once the agent has shown `config_pending` and a later restart lands,
      the feedback converges to `Saved. Changes are in effect.` — including
      the Allow & Restart auto-restart path
- [x] changing the username while a password is stored requires an explicit
      `Edit…` first; Save never decrypts merely to migrate it. After Edit, Save
      creates the new account item, removes the old item, and clearly asks for
      a new OpenCodeServerAgent grant
      - The installed walkthrough first produced the explicit Edit guidance,
        then completed the migration and authorization without a second restart
        alert. The original username and credential were restored afterward.
- [x] choosing `Remove…` and saving deletes the item and returns to
      `Not configured`; the non-interactive `credential_removed` notice drops
      any old in-memory credential, and OpenCode restarts unauthenticated by
      user choice
      - Build 62 installed-machine defect, 2026-08-09: after the explicit
        non-loopback `Save Anyway`, the Generic Password item was deleted
        (`security find-generic-password` returned item-not-found), but
        OpenCodeServerAgent remained in `password_state: access_pending` for
        repeated status observations instead of converging to
        `not_configured`. The verified old PID `91083` correctly stayed
        supervised with its old credential while restart was deferred, but
        Settings simultaneously rendered the empty password editor with
        `Agent access: Granted`, contradicting both the agent state and the
        absent item. Restart was intentionally not attempted because the
        `access_pending` start guard could turn an explicit password removal
        into a refused start. Build 63 introduced the distinct
        `credential_removed` path, direct `not_configured` convergence, and a
        deterministic regression that also discards a decrypt result already
        in flight when deletion occurs. Build 66 `macOS 3.utm` evidence closes
        this gate: the item was deleted, the Agent reported `not_configured`,
        and OpenCode received one SIGTERM before restarting healthy in native
        unauthenticated mode.
- [ ] Instruments/Main Thread Checker and the macOS runtime log show no
      Security.framework query, decrypt, add, update, or delete on the AppKit
      main thread; delayed securityd responses affect only the bounded Settings
      worker operation
- [x] atomic-replacement upgrade (same signing identity, new cdHash) lands in
      `access_pending` on first agent start: the persisted grant marker is
      bound to the previous bundle version and MUST NOT silently authorize the
      unproven new binary. This is the required conservative behavior for the
      macOS 26.5.1, self-signed/no-Team-ID, login-keychain version-512
      configuration validated by the isolated 2026-08-09
      KeychainPartitionProbe experiment (ADR 0016), not a universal statement
      about every file-based keychain. One “Allow Keychain Access…” click
      completes the re-authorization; NO background prompt appears at any point
      - Build 63 → 64 preserved OpenCode PID `11340` while authenticated
        protocol-5 status reported `access_pending` and `config_pending` for
        the unproven Build 64. OpenCodeServerAgent attempted no background
        decrypt; the user's later explicit grant returned the installed app to
        authenticated `healthy`.
- [ ] during that upgraded first start, the deferred credential read runs on
      the single-flight worker, never inline on the supervisor event loop:
      process supervision and the SMAppService registration transaction
      complete normally even if a consent dialog is left unanswered
- [ ] editing the item’s value in Keychain Access (wild change) produces
      the dedicated HTTP 401 guidance, not a crash or silent failure
      - Build 66 `macOS 3.utm`: the item was externally edited while the Agent
        remained `configured / healthy`; the required HTTP 401 guidance was
        not exercised, so this gate remains open.
- [ ] deleting the item in Keychain Access while OpenCode runs produces the
      “removed from Keychain” guidance
- [ ] `security scan`, Unified Logging, `state.json`, and process lists
      never contain the password

## Local Network privacy and mDNS

Build 62 installed-machine testing on 2026-08-09 found a
privacy-attribution mismatch that was initially treated as release-blocking.
After the user explicitly enabled mDNS and applied
the setting by restarting OpenCode, macOS displayed its Local Network alert as
`OpenCodeServerA` with a generic network icon and generic copy. The string is a
truncated fallback derived from the stable `OpenCodeServerAgent` process name,
not an unknown application, but it does not provide the meaningful responsible-
app identity required by the product. The generated main-app Info.plist also
lacked `NSLocalNetworkUsageDescription`.

Build 63 added the same concise `NSLocalNetworkUsageDescription` to the
Xcode-generated main-app Info.plist and OpenCodeServerAgent's documented
`__TEXT,__info_plist` fallback, while retaining the Service Management
`AssociatedBundleIdentifiers` entry for `ai.opencode.server`. Bundle validation
fails if any of those identity or purpose strings is absent. Build 64 retains
those validated metadata requirements. Metadata is still not proof of the
responsibility chain: the resulting prompt and System Settings identity must
be observed on clean Local Network privacy state.
No `NSBonjourServices` value is guessed without first identifying an actual
Bonjour service type in the supported OpenCode version.

### VM evidence (2026-08-12)

The clean baseline was cloned into two stopped VMs with distinct MAC
addresses. The Build 64 experiment group (`OpenCodeServer 实验组`,
`192.168.64.5`) installed the current signed app and OpenCode 1.18.16. With
mDNS initially off, no Local Network prompt appeared. After enabling mDNS,
changing the listener to the VM's non-loopback address, saving, and restarting
OpenCode, the saved screenshot was initially misread as a correct
`OpenCodeServer` prompt. Reinspection shows that the title was actually
`OpenCodeServerA`; the icon and copy were also the generic fallback. Allowing
it left the OpenCodeServerAgent/OpenCode health state healthy. Returning to
`127.0.0.1`, disabling mDNS, saving, and restarting produced no second prompt.

The Build 62 control group (`OpenCodeServer 对照组`, `192.168.64.6`) was built
from the pre-fix revision. The same mDNS flow reproduced the old prompt title
`OpenCodeServerA`; System Settings listed the Local Network entry as
`OpenCodeServerAgent`.

The initial Build 64 attribution observation was also misread: the saved prompt
shows `OpenCodeServerA`, while the Local Network list shows
`OpenCodeServerAgent`. The subsequent single-version app/config retest
reproduced the same pair. The structural prerequisites are checked separately
below; under the tested self-signed/no-Team-ID plus external-child
architecture, the UI result is recorded as a platform limitation rather than
as a remaining bundle-structure repair task.

### Clean single-version retest (2026-08-12)

To test whether the TN3179 multiple-version condition was the cause of the
System Settings discrepancy, `macOSvm3` was duplicated to a separate stopped
VM (`OpenCodeServer 单一版本实验组`, UUID
`C3B3BFFC-2C98-4380-810E-8E618078B7BB`, MAC `5a:bb:2c:f6:53:01`). The clone
started with no OpenCodeServer application, configuration, or registered
OpenCodeServerAgent. Only the signed Build 64 app and OpenCode 1.18.16 were
installed; Build 62 and any other OpenCodeServer version were absent.

With the listener changed to `192.168.64.7`, mDNS enabled, and the user-
initiated restart completed, the first Local Network alert again named
`OpenCodeServerA`. The saved screenshot shows the truncated title, generic
network icon, and generic copy rather than the intended `OpenCodeServer`
responsible-app identity.
After choosing Allow, System Settings → Privacy & Security → Local Network
still contained a single `OpenCodeServerAgent` row (toggle off); no
`OpenCodeServer` row appeared. The VM was then restored to `127.0.0.1` with
mDNS disabled, returned to healthy status, and stopped.

This clean app/config, single-version run does not support treating Build 62
and Build 64 coexisting as the sole explanation for the System Settings
attribution discrepancy. It narrows the investigation but does not identify
the remaining macOS responsibility-chain cause, and it does not close the
acceptance gate. Because the VM was cloned from the macOSvm3 baseline rather
than from an erased TCC database, the pre-existing Local Network row was not
independently proven absent before installation; this result cannot establish
that coexistence is unnecessary in every privacy-history state.

### Agent-only attribution diagnostic (Build 66, 2026-08-12)

Build 66 was compiled with the diagnostic-only
`diagnostic-local-network` Cargo feature. Before any external OpenCode child
was launched, OpenCodeServerAgent bound a UDP socket, joined
`224.0.0.251`, sent a four-byte packet to port 5353, waited 300 ms, and ended
the probe worker. The feature is excluded from normal product builds.

The probe ran in a new clone of the macOSvm3 baseline:
`OpenCodeServer Agent Local Network 诊断`, macOS 26.6.1 (25G76), Apple Silicon,
self-signed/no-Team-ID, Build 66. The Agent binary had a non-empty unique
arm64 `LC_UUID` (`F3BBF6B5-A96B-3076-B232-DEAD930A2716`).

The system alert still displayed **`OpenCodeServerA`**. The VM's Unified Log
recorded:

```text
nehelper: No team ID found for (bundleID: ai.opencode.server.agent, name: OpenCodeServerA)
nehelper: Found path /Applications/OpenCodeServer.app/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent for PID 1227, will prompt
nehelper: Local network preference not yet set, prompting for OpenCodeServerA (ai.opencode.server.agent)
OpenCodeServerAgent: Diagnostic Local Network probe failed: No route to host (os error 65)
```

The first diagnostic attempt only joined the multicast group and produced no
prompt; it was not treated as evidence. The Build 66 attempt emitted actual
multicast traffic and produced the prompt. The send's `ENETUNREACH` result
occurred after the system entered the prompt path and does not change the
responsible-code observation.

This falsifies the narrow hypothesis that the attribution problem is caused
only by an external OpenCode grandchild. In the tested self-signed/no-Team-ID
environment, an Agent-originated operation is also presented as the truncated
Agent identity. This supports the signing-strength/identity-chain hypothesis;
an Apple-issued signing model or another supported responsibility-chain change
is required before testing whether upward attribution can work. It does not
prove that Developer ID alone fixes every external-child case or establish a
universal rule for other signing or macOS configurations.

### Structural attribution prerequisites (2026-08-12)

The two low-cost checks for the Local Network responsibility chain both passed:

- `[x]` `launchctl print gui/501/ai.opencode.server.agent` reported
  `managed_by = com.apple.xpc.ServiceManagement`, `parent bundle identifier =
  ai.opencode.server`, and `parent bundle version = 64`. SMAppService therefore
  associated the LaunchAgent with the main app; no separate delegate-app field
  was exposed in the record.
- `[x]` `dwarfdump --uuid` found arm64 `LC_UUID` values for the installed
  Build 64 OpenCodeServerAgent (`94A1D912-4D76-3FB6-A0E1-2F5F33F85A12`), the
  Build 65 Release candidate Agent (`7FDE535D-5581-3571-AAF0-C773B98E156A`),
  the installed Build 64 OpenCodeServer (`B5A304EC-4BCB-3FC1-AC11-D44EA580B0D0`),
  the Build 65 candidate OpenCodeServer (`25622A5F-0737-3653-AFC4-04B2ACA41B78`),
  and external OpenCode 1.18.16 (`C7E7A979-F99B-3466-9AD6-E56A63373A35`).
  No duplicate UUID appeared in the tested product-binary set. Two clean Rust
  Release builds with identical `OPENCODESERVER_BUNDLE_VERSION=65` input both
  produced Agent UUID `5D1914AA-A658-3BFA-AAAC-3F2A4CDED0E7`; Build 64 and
  Build 65 produced different UUIDs.

These are now the Local Network release prerequisites. The observed
`OpenCodeServerA`/`OpenCodeServerAgent` UI names are not evidence of a missing
SMAppService association or a missing/duplicated `LC_UUID`. In this
architecture with an external OpenCode child, exact upward UI attribution is
not promised. The signing-model re-run is done: the 2026-08-16 dual-group
clean-state experiment (ADR 0018 amendment; ADR 0021 post-implementation
measurement) observed the alert and the System Settings row named
`OpenCodeServerAgent` under BOTH the outgoing self-signed and the current
Apple Development identity, so a signing-model change alone no longer
re-opens this UI gate — the remaining trigger is a responsibility-chain
change. Do not keep adding bundle-naming or purpose-string patches.

Apple baselines:

- [TN3179: Understanding local network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
- [`NSLocalNetworkUsageDescription`](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSLocalNetworkUsageDescription)
- [Xcode Build Setting: Create Info.plist Section in Binary](https://developer.apple.com/documentation/xcode/build-settings-reference)

- [x] with clean per-user Local Network privacy state and mDNS off, launching,
      opening Settings, and restarting OpenCode raise no Local Network alert
      (Build 64 experiment group; no alert before mDNS was enabled)
- [x] enabling mDNS and applying it raises one alert only after the explicit
      user choice and OpenCode restart (Build 64 experiment group, macOS
      26.6.1; the alert was observed, but its attribution was incorrect)
- [x] the structural attribution prerequisites are present: SMAppService
      records `ai.opencode.server` as the parent bundle, all tested native
      executables are arm64 with present and unique `LC_UUID` values, and
      identical Build 65 inputs produce a stable Agent UUID (see the evidence
      subsection above)
- [x] Local Network UI behavior is recorded accurately for both signing
      models: the single-version Build 64 screenshot (self-signed) showed
      `OpenCodeServerA`, and System Settings listed `OpenCodeServerAgent`; the
      2026-08-16 dual-group clean-state re-test (ADR 0018 amendment) observed
      the alert title and the System Settings row named `OpenCodeServerAgent`
      under both the outgoing self-signed and the current Apple Development
      identity. This is a platform limitation of the external-child
      responsibility chain, not a claim that UI attribution passed; reopen the
      UI check only if the responsibility chain changes
- [x] after an Apple-issued signing model or a changed responsibility chain is
      introduced, repeat the clean-state alert and System Settings attribution
      check
      - Done 2026-08-16 (ADR 0018 amendment; ADR 0021 post-implementation
        measurement): dual-group clean-VM experiment, four interleaved rounds
        differing only in signing identity; the alert title and the System
        Settings row are named `OpenCodeServerAgent` under BOTH identities,
        with the SMAppService parent record and unique arm64 `LC_UUID`
        prerequisites holding in every round. Signing identity is not the
        discriminator; this gate re-opens only on a responsibility-chain
        change.
- [ ] choosing Allow makes the supported OpenCode mDNS name discoverable while
      preserving authenticated health; choosing Don’t Allow fails without a
      crash, hang, background re-prompt loop, or misleading healthy mDNS claim
      (the Build 64 experiment group remained healthy after Allow, but the
      single-version alert was incorrectly attributed; the clean
      deny branch is still open; the separate Build 62 control group reproduced
      the old `OpenCodeServerA` prompt and its OpenCodeServerAgent Local Network
      entry)
- [x] disabling mDNS and restarting stops advertisement and does not request
      Local Network access again (Build 64 experiment group returned to
      `127.0.0.1`, `MDNS=false`, and healthy status without another prompt)
- [ ] repeat the allow and deny paths on a fresh user or restorable VM; current
      macOS does not provide a reliable general-purpose reset for this privacy
      state, so an existing grant is not clean-state evidence
      (the single-version restorable-VM Allow path and the old-defect control
      comparison are recorded; a clean deny branch remains open)

## Process and network behavior

- [ ] direct child and native descendants share a dedicated process group
- [ ] graceful stop terminates the whole tree
- [ ] a TERM-ignoring fixture reaches `StopTimedOut` without receiving KILL
- [ ] Continue Waiting extends the interval
- [ ] explicit Force Stop kills only the revalidated group
- [x] crash recovery follows approximately `1, 2, 5, 15, 30` seconds
      - Build 63 installed-machine evidence, 2026-08-10: one external SIGTERM
        terminated OpenCode PID `653`; OpenCodeServerAgent entered
        `waiting_to_restart` with “attempt 1 of 5 ... in 1 seconds”, started
        replacement PID `11340` about 1.05 seconds later, and reached `healthy`
        about one second after spawn.
      - Build 64 installed-machine evidence, 2026-08-10: after the stable-run
        reset, external SIGTERM ended baseline PID `29573`. The harness observed
        attempts 1–5 scheduled at `1, 2, 5, 15, 30` seconds and terminated only
        the authenticated replacement PIDs `31145`, `31420`, `32085`, `34060`,
        and `38024`. OpenCodeServerAgent then entered `failed` with the exact
        five-attempt exhaustion error. A normal Start restored one healthy
        OpenCode as PID `38177` on the configured endpoint.
- [x] only first failure, recovery, and exhausted recovery notify
      - Build 63 historical failure, 2026-08-10: OpenCodeServerAgent correctly
        emitted exactly event `1/failure` and
        event `2/recovered` for that incident. The installed OpenCodeServer did
        not deliver their banners: `LastNotificationEventID` was still `82`,
        so `NotificationController` incorrectly treated the freshly reset
        OpenCodeServerAgent event IDs as old.
      - Build 64 installed-machine pass, 2026-08-10: one external SIGTERM
        terminated PID `11340`; OpenCodeServerAgent started PID `29573` and
        recovered to `healthy`. It emitted failure event
        `ca670024-2193-4489-a8ea-12d11fce139b` and recovery event
        `8590befb-7c1b-401a-91c5-9c7bccf1deb0`. OpenCodeServer submitted both
        globally unique IDs, Unified Logging recorded both notification
        requests as accepted without error, and the user confirmed both
        “OpenCode stopped unexpectedly” and “OpenCode recovered” in Notification
        Center.
      - Build 64 five-attempt exhaustion pass, 2026-08-10: the entire incident
        retained one failure event
        `56f8741e-f28f-4c4d-b306-1cac3196315a` across all five attempts, then
        emitted one final-failure event
        `e2afc648-00e6-402a-b450-f493a9668f48`. The accepted-ID ledger gained
        exactly those two IDs, Unified Logging recorded exactly two successful
        submissions, and the user confirmed Notification Center contained only
        “OpenCode stopped unexpectedly” and “OpenCode recovery stopped” for the
        incident: no intermediate duplicate and no incorrect recovery notice.
- [ ] three health failures turn state yellow but do not restart a live process
- [x] configured endpoint is the only OpenCode listener
      - Build 64 installed checks before and after both recovery incidents
        found exactly one OpenCode process and one listener on
        `10.0.0.254:4096`; after the exhaustion test the restored PID was
        `38177`.
- [ ] IPv6 installed behavior: `::1` starts, formats as `[::1]:<port>`, remains
      classified as loopback, and passes authenticated/no-password health as
      configured; one selected non-loopback IPv6 address binds only that
      address and receives the same unauthenticated-listener warning as IPv4
- [ ] a foreign listener causes a port-conflict error and is never terminated
- [ ] Restart rides out the predecessor's endpoint-release window instead of
      failing with a spurious port conflict (ADR 0011)
- [ ] an identity-verified but configuration-mismatched process is taken over
      as managed (stoppable/restartable, reported `config_pending`), rechecked
      once credentials converge, and replaced by a correctly configured child
      on Restart; an identity-MISMATCHED record stays fail-closed and is never
      signaled (ADR 0005 2026-08-05 amendment)
- [x] password and no-password health checks both pass
      - Build 66 `macOS 3.utm`: authenticated `configured / healthy` status
        was observed after `Always Allow`; after removal and final restart,
        `not_configured / healthy` status and the same endpoint were observed.

## Menu layout and progressive disclosure

Baselines: `~/Documents/OpenCodeServer-References/` (NN/g progressive
disclosure, HIG Menus, HIG The Menu Bar, HIG Onboarding).

- [x] healthy steady state shows exactly four status rows — OpenCode health,
      Uptime, Listening, OpenCode version — plus the stable action set; the
      OpenCodeServerAgent, FDA, Password, Authentication, and Configuration
      rows are all hidden
      - The installed healthy-state walkthrough and menu capture showed exactly
        these four informational rows before the action separator.
- [ ] each conditional row appears while (and only while) it deviates:
      non-nominal registration, FDA not Verified, Keychain access pending,
      unauthenticated non-loopback listener, configuration pending/error
- [ ] `Detail:` appears for the current actionable `config_error`, otherwise
      for the current actionable `last_error`, and disappears as soon as the
      error clears
- [ ] with OpenCodeServerAgent unreachable, ALL conditional rows are visible
      (stale values must not read as a healthy state)
      - Build 62 installed-machine observation, 2026-08-09: disabling
        OpenCodeServer under System Settings > General > Login Items &
        Extensions correctly removed the OpenCodeServerAgent launchd job and
        IPC socket while leaving the verified OpenCode PID `78848` listening.
        The menu correctly showed `OpenCodeServerAgent Temporarily
        Unavailable`, `Requires Approval`, em dashes for runtime/FDA/config
        data, the complete stable action set, and disabled inapplicable
        actions. However, it incorrectly rendered `Password: Not configured`
        and `Authentication: Not enabled` even though the installed
        configuration had a granted credential and authenticated listener.
        Current source renders both missing values as `Unable to determine` and
        hosted tests cover the nil-status labels and all-row visibility. This
        is no longer a known source defect; the checkbox remains open solely
        for an installed Build 64 disable/re-enable observation.
- [x] the action set is identical in every state — Start/Stop/Restart,
      Continue Waiting, Force Stop, Settings…, Advanced, and the two quit
      actions are disabled when inapplicable, never hidden
      - Installed healthy and OpenCodeServerAgent-unreachable captures showed
        the same action set with only applicability changing.
- [x] rarely used actions live in a single-level `Advanced` submenu with
      exactly five items: Open Logs, Recheck Full Disk Access, Open Full
      Disk Access Settings, Open Login Items Settings, Repair
      OpenCodeServerAgent…
      - The installed menu capture confirmed one submenu and exactly these five
        actions.
- [ ] the very first launch opens the Settings window once; quitting and
      relaunching never opens it again (UserDefaults flag)
- [ ] the Settings `Advanced` disclosure hides mDNS and executable selection
      by default, expands automatically when the loaded configuration uses a
      non-default value, and the window resizes to fit in both directions

## Accessibility and menu behavior

- [x] Main storyboard contains Application, Edit, and Window menus
- [x] Edit commands have nil targets and standard selectors, so AppKit routes
      them through the current first responder
- [x] Settings uses native `NSTextField` for Listening address, Port, Username,
      revealed Password, and OpenCode executable, plus `NSSecureTextField` for
      the concealed Password
- [ ] About OpenCodeServer, Settings… (⌘,), Services, Hide, Hide Others, Show
      All, and Quit OpenCodeServer (⌘Q) behave normally in the installed app
- [ ] Listening address: ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, and ⇧⌘Z work
- [ ] Port: ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, and ⇧⌘Z work where the number formatter permits
- [ ] Username: ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, and ⇧⌘Z work
- [ ] concealed Password: standard secure-field editing works, including
      paste, select-all, undo, and redo; AppKit security restrictions on
      copying concealed text remain intact
- [ ] revealed Password: ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, and ⇧⌘Z work
- [ ] OpenCode executable: ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, and ⇧⌘Z work
- [ ] status has both color and text in every state
- [ ] VoiceOver announces the menu bar item and every status/action
- [ ] keyboard navigation reaches all menu and Settings controls
- [ ] system contrast settings preserve status readability
- [x] the real password length is never revealed anywhere in the UI
      - Installed menu and Settings observations showed only state text until
        the user explicitly entered Edit; no menu/status row exposed a
        credential-derived length.
- [x] password reveal and copy require explicit settings-window actions
      - The installed walkthrough confirmed that Show exists only in explicit
        Edit state and Copy performs its decrypt only after the Copy action.
- [ ] FDA, authentication, config pending, and version pending do not alter a
      healthy green OpenCode state

## FDA and responsible code

Use a clean VM or a restorable clean TCC state.

- [ ] before FDA, Safari history probe returns `Not Verified` or, if the exact
      target is absent, `Unable to Determine`
- [x] after FDA for `/Applications/OpenCodeServer.app`, the
      OpenCodeServerAgent probe returns
      `Verified`
      - Build 64 authenticated status remained `fda: verified` across the
        installed update, OpenCode PID reattachment, both recovery incidents,
        and the final normal Start.
- [ ] probe reads no content and logs no protected path
- [ ] removing FDA returns to a non-positive state
- [ ] AttributionChain shows:

```text
responsible = ai.opencode.server
accessing   = /opt/homebrew/Cellar/opencode/<version>/bin/opencode
```

- [ ] if attribution points to the Cellar binary, release is blocked

## File Provider and upgrade persistence

- [ ] first intended Dropbox/File Provider access produces at most the expected
      one-time consent
- [ ] replace OpenCode with a different Homebrew version/hash
- [ ] restart through OpenCodeServer
- [ ] the same access succeeds without a new FileProviderDomain prompt
- [ ] install build N+1 signed by the same stable identity
- [ ] FDA and provider access persist

## Signing and rollback

- [x] ad hoc structural build signs nested OpenCodeServerAgent and
      opencodeserverctl before OpenCodeServer
- [x] ad hoc structural build has Hardened Runtime with no exception
      entitlements and no App Sandbox
- [x] stable integration identity build has Hardened Runtime with no exception
      entitlements and no App Sandbox
- [x] build N and N+1 satisfy each other’s Designated Requirements using
      `scripts/check_requirements.sh`
- [x] a failed install restores the previous bundle into place, and a failed
      restore preserves the staging directory holding the previous bundle for
      manual recovery (fault-injection tested; a successful install keeps no
      historical app copy)

## Operational documentation

- [x] document the full signing identity lifecycle: creation, trust scope,
      private-key ACL, expiration/renewal, secure backup, and distribution of
      only the public certificate to other test Macs
      - the signing-identity runbook is maintained outside the repository
        (in-repo template: `docs/signing-identity.example.md`); it covers
        the active ADR 0021 Apple Development identity, including the yearly
        reissue cadence and the no-`.p12` private-key policy; the retired
        self-signed `OpenCodeServer Local Signing` identity is retained there
        for rollback/reference only.
