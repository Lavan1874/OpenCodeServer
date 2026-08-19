# OpenCodeServer — Agent Development Instructions

## Scope

These instructions apply to the entire OpenCodeServer repository.

OpenCodeServer is a small, native macOS utility that keeps a Homebrew-installed
OpenCode running and exposes concise status and controls through a menu bar
item. It must remain understandable, auditable, low-overhead, and
faithful to macOS platform conventions.

The product name is **OpenCodeServer**.

The first-stage deployment target is **macOS 26 on Apple Silicon**. Do not add
Intel support or compatibility code for older macOS releases unless the product
decision changes.

Before planning or implementation, read `PRODUCT_DECISIONS.md`. It is the
authoritative concise record of accepted product behavior. If an older
statement in this file or any other repository document conflicts with it,
follow `PRODUCT_DECISIONS.md` and update the stale documentation.

## Prime directive: Apple-first engineering

OpenCodeServer MUST be designed as a first-class macOS application, not as a
Unix tool hidden inside an arbitrary wrapper.

For every decision involving app bundles, lifecycle, Service Management,
launchd, code signing, TCC, privacy, background execution, AppKit, Swift,
accessibility, menu bar behavior, or macOS networking:

1. Consult current primary Apple documentation first, preferably
   `developer.apple.com`.
2. Check API availability for the minimum supported macOS version.
3. Prefer documented public Apple APIs and supported bundle layouts.
4. Record important platform decisions and their Apple sources in the design
   documentation or an ADR.
5. If Apple provides no supported API, state the limitation honestly. Do not
   present heuristics as authoritative system state.

Do not base product behavior on blog posts, Stack Overflow answers, reverse
engineering, private frameworks, undocumented database schemas, or parsing
diagnostic CLI output when a supported Apple API exists.

The UI MUST follow Apple’s Human Interface Guidelines, including menu bar
ergonomics, terminology, keyboard behavior, VoiceOver/accessibility, spacing,
contrast, and the rule that color alone must not communicate status.

## Fixed product architecture

OpenCodeServer is one standard macOS `.app` with two long-running native
product components:

- `OpenCodeServer`: Swift/AppKit menu bar GUI.
- `OpenCodeServerAgent`: Rust background manager supervised by launchd.

The app also contains the auxiliary native `opencodeserverctl` client required
by `PRODUCT_DECISIONS.md`. It is never a long-running component and does not
alter the two-process runtime architecture.

OpenCodeServerAgent launches and supervises one user-selected native OpenCode
executable. The default discovery order includes the standard Homebrew and
OpenCode installation locations.

OpenCodeServer and OpenCodeServerAgent are separate processes but one signed
product. Quitting OpenCodeServer unexpectedly MUST NOT stop OpenCode.
“Quit OpenCodeServer”, “Stop OpenCode”, and “Stop OpenCode and Quit
OpenCodeServer” are distinct, clearly labelled actions.

The production bundle identifiers are:

- Main app: `ai.opencode.server`
- OpenCodeServerAgent: `ai.opencode.server.agent`

## Permanent component terminology and authority

Use these exact proprietary names in discussion, reports, documentation, UI,
Unified Logging, comments, and test names:

- `OpenCodeServer`: the Swift/AppKit GUI, settings editor, Service Management
  registration coordinator, and IPC client.
- `OpenCodeServerAgent`: the launchd-managed Rust OpenCode manager,
  supervisor, runtime-state authority, FDA probe owner, and IPC server.
- `opencodeserverctl`: the short-lived command-line IPC client.
- `OpenCode`: the external Homebrew-installed process that provides the actual
  service.

Do not shorten these names to “Server”, “menu bar app”, “Agent”, “background
app”, or “CLI” when referring to one of these components. Generic API terms
such as Apple’s `LaunchAgent`, `SMAppService.Status`, or an OpenCode product
concept named “Agent” retain their official spelling.

The responsibility boundary is strict:

- OpenCodeServer may present status, edit configuration, send IPC commands,
  monitor status over IPC (pushed subscription with bounded reconnect, ADR
  0010), and manage `SMAppService` registration only for first
  registration, a real bundle-version change, a verified registration error,
  or an explicit “Repair OpenCodeServerAgent” action.
- OpenCodeServer MUST NOT inspect or signal OpenCode PIDs, supervise OpenCode,
  infer registration corruption from IPC unavailability, or automatically
  unregister/re-register OpenCodeServerAgent during ordinary status
  monitoring.
- OpenCodeServerAgent is the only OpenCode process manager and the only
  authority for OpenCode runtime state.
- opencodeserverctl uses the same IPC protocol and MUST NOT manage OpenCode
  processes or runtime-state files directly.
- OpenCode is independent and has no knowledge of OpenCodeServer,
  OpenCodeServerAgent, or opencodeserverctl.

At login, macOS launches OpenCodeServer and OpenCodeServerAgent independently.
No startup ordering is assumed. If an already-enabled, same-version
OpenCodeServerAgent is temporarily unreachable, OpenCodeServer keeps
monitoring IPC and presents “Starting” or “Temporarily Unavailable”; it does
not mutate Service Management registration or `RegisteredBundleVersion`.

A real bundle-version update is one persistent, bounded transaction, not
ordinary status monitoring. Because macOS 26 can accept a changed embedded
LaunchAgent registration while retaining stale launch metadata, the transaction
may make at most three state-observed unregister/register attempts. Each
attempt must wait for asynchronous unregistration, observe `notRegistered`,
allow bounded settling, and require authenticated OpenCodeServerAgent IPC.
Persist the attempt number so an OpenCodeServer restart cannot reset the
transaction budget. Never apply this retry behavior to an enabled,
same-version registration.

Development variants MAY use a `.dev` suffix and a different port so they can
be tested without disrupting the production service.

## Native implementation rules

### OpenCodeServer

- Use Swift and public AppKit APIs.
- Prefer `NSStatusItem` and a native `NSMenu` for the first version.
- Use `LSUIElement` so the app behaves as a menu bar utility.
- Do not introduce Electron, Tauri, Node.js, a web view, or a bundled browser.
- Add SwiftUI only when it materially improves a future settings window or
  popover; do not use it merely for novelty.

### Xcode project baseline

- `OpenCodeServer.xcodeproj` is the only Swift build definition and must remain
  directly loadable by Xcode 26 without XcodeGen, Tuist, or another generator.
- Keep a real macOS Application Target, hosted XCTest Target, shared Scheme,
  Debug/Release configurations, and text-reviewable common settings in
  `Config/*.xcconfig`.
- Xcode owns Swift/AppKit compilation, generated bundle metadata,
  `LSUIElement`, resources, the asset catalog, entitlements, architecture and
  deployment settings, the outer app signature, tests, and archives.
- Keep the standard Main storyboard Application/Main Menu scene. The Settings
  window may remain programmatic AppKit.
- Standard Edit menu commands MUST target the current first responder. Do not
  replace the Responder Chain with per-field clipboard code or keyboard event
  interception.
- The Cargo integration is an Xcode Run Script phase with declared input and
  output file lists. It must build, copy, and sign OpenCodeServerAgent and
  opencodeserverctl before Xcode signs the outer app, and any failure must fail
  the Xcode build.
- `scripts/build.sh` may wrap `xcodebuild` and copy a complete signed product,
  but it must not assemble or mutate the app after Xcode’s CodeSign phase.
- Do not restore a parallel `Package.swift` unless it gains an independently
  documented purpose that cannot be served by the Application and XCTest
  Targets.

### OpenCodeServerAgent

- Use stable Rust with a small dependency set.
- Do not use Tokio or a general async runtime unless a measured requirement
  justifies it.
- Prefer `std` plus narrowly scoped, well-maintained crates such as `rustix` or
  `nix` for POSIX integration and `serde` for bounded status messages.
- Keep all `unsafe` code isolated, documented, reviewed, and tested.
- The target characteristics are fast startup, near-zero idle CPU, and a small
  resident memory footprint.
- Keep the informational installed-version query isolated from the main
  supervisor. It must remain single-flight, bounded, and dispensable; do not
  add a wrapper, guardian hierarchy, descendant ledger, or pending-cleanup
  state machine to improve an informational label.

### Process supervision

OpenCodeServerAgent MUST:

- execute the configured absolute native Mach-O path directly;
- never insert a shell or script interpreter into the execution chain;
- create a dedicated process group for OpenCode and its descendants;
- forward termination signals to the entire child process group;
- treat that process group as cooperative lifecycle management for a trusted
  OpenCode installation, not as hostile-code containment;
- allow a bounded graceful-shutdown interval;
- never send `SIGKILL` merely because the graceful deadline expired; force
  termination requires a second explicit user action or opencodeserverctl
  `--force`;
- reap the direct child correctly and handle interrupted waits;
- distinguish normal exit, signaled exit, explicit stop, and startup failure;
- avoid PID-reuse races and clean descendants that remain in the authorized
  process group;
- on reattachment, classify the recorded process identity before comparing
  configuration; clear missing or identity-mismatched records without signaling
  them, but preserve records when inspection itself is inconclusive;
- use the versioned canonical configuration fingerprint defined by ADR 0005,
  not persistent `st_dev`/inode/mtime/length equality, and recheck the complete
  process identity after authenticated health verification;
- an identity-VERIFIED but configuration-mismatched process is not
  abandoned: OpenCodeServerAgent takes it over as a managed
  stale-configuration process (stoppable and restartable, reported as
  `config_pending`), and rechecks it against the new configuration once
  credentials converge; a restart then replaces it with a correctly
  configured child (ADR 0005, 2026-08-05 amendment). Identity mismatch
  stays fail-closed as before;
- use synchronous signal consumption such as blocked signals plus `sigwait`,
  rather than running complex Rust code inside an asynchronous signal handler.

OpenCodeServer v1 does not promise containment of a deliberately hostile
native executable that escapes its process group/session after the last
trustworthy snapshot. Keep observed escape and identity errors fail-closed:
never signal a foreign group or inferred descendant PID. For the informational
installed-version query, an observed escape or identity anomaly also opens an
automatic retry circuit breaker for that configured executable until its path
changes or OpenCodeServerAgent restarts. Do not introduce EndpointSecurity,
additional entitlements, a wrapper, or a more elaborate guardian to chase the
post-snapshot escape window without a new product decision and ADR.

## Service Management and bundle layout

- Use a standard Apple app bundle layout.
- All executable bundle entry points MUST be native Mach-O binaries.
- Embed the LaunchAgent property list in the app bundle.
- Use `SMAppService.agent(plistName:)` to register and unregister
  OpenCodeServerAgent on macOS 13 and later.
- Do not manually install the production plist into
  `~/Library/LaunchAgents`.
- Include `AssociatedBundleIdentifiers` in the embedded LaunchAgent plist so
  macOS can associate OpenCodeServerAgent with `ai.opencode.server`.
- Keep mutable state, logs, sockets, and configuration outside the signed app
  bundle.
- Never modify a signed bundle in place. Build and sign a staging bundle,
  validate it, then replace the installed bundle atomically.

## Code signing identity

During local development and local Release acceptance, sign integration
builds with the current ADR-designated Apple Development Code Signing
identity on the designated build Mac. Ad hoc signing is acceptable only for
short-lived unit/process tests that do not exercise TCC.

- Keep the signing private key on one designated build Mac. Other personal test
  Macs receive already-signed builds and the public certificate only.
- Keep bundle identifiers stable.
- Sign nested code first and the outer app last.
- Verify every candidate with `codesign --verify --deep --strict`.
- Dump and compare Designated Requirements for upgrade candidates.
- For same-identity upgrades, verify that build N and build N+1 satisfy each
  other’s expected requirements. For a signing-identity migration governed by
  an accepted ADR, use the explicit migration gate to verify the exact
  outgoing and incoming leaf authorities, the candidate's own requirement,
  and record the one-way Designated Requirement transition.
- Treat any signing-identity change, including migration to Developer ID, as a
  change that may require one-time privacy reauthorization.
- Never weaken the Designated Requirement to “bundle identifier only”.
- The active Apple Development identity, its Apple-rooted trust scope,
  private-key ACL, expiry cadence, reissue procedure, loss recovery, and the
  planned Homebrew-tap distribution path are documented in the
  maintainer-local signing-identity runbook
  (`~/Documents/OpenCodeServer-References/signing-identity.md`; in-repo
  template: `docs/signing-identity.example.md`); keep that runbook in sync
  whenever the identity or its trust scope changes. The concrete identity
  name for build/install defaults lives only in
  `~/.config/opencodeserver/signing-identity` and is never committed.

## TCC, FDA, and File Provider rules

OpenCodeServer is intentionally not App Sandbox-enabled because its core
purpose is to launch developer tooling that must work with arbitrary
user-selected projects. Keep other entitlements minimal.

The product MUST NOT:

- grant or modify privacy permissions programmatically;
- write to a TCC database;
- parse a TCC database as product logic;
- inspect System Settings through UI automation;
- use private TCC APIs;
- claim that a settings row or database row proves effective access;
- scan or maintain a list of OpenCode project directories.

The menu displays Full Disk Access as a tri-state:

- `Verified`
- `Not Verified`
- `Unable to Determine`

FDA verification MUST be performed by `OpenCodeServerAgent`, because that is
the process responsible for launching OpenCode. A positive result must come
from a minimal, read-only functional probe against an item that is known to
require FDA on the target macOS version. Do not read or retain user content.
ADR 0002 (2026-08-20 amendment) defines the probe as version-gated and
consensus-based: only macOS 26.x probes, over the versioned target set
`~/Library/Safari/History.db`, `~/Library/Mail/V10`, and
`~/Library/Suggestions` (per target: read-only open, metadata inspection,
immediate close, no content read; `stat` for existence only — it is not
TCC-gated and must never be the access test). Every other OS version —
including macOS ≥27, where every classic FDA-protected path measured on
27.0 beta 26A5416b was readable without FDA — reports
`Unable to Determine` without probing. Keep the target table covered by
versioned tests; changing it requires a new documented decision and, for a
new OS major version, a clean-state on-metal A/B re-measurement. A failed
probe is not conclusive proof that the FDA switch is off.

Do not display or proactively probe FileProviderDomain status in the first
version. The current product assumption is that verified FDA is sufficient for
the intended File Provider paths. This is an assumption, not a platform
guarantee; if real acceptance testing disproves it, revisit the design. For
Local Network prompts, verify the signed responsibility-chain prerequisites
before interpreting UI attribution: the SMAppService record must identify
`ai.opencode.server` as the parent bundle, and every participating arm64
Mach-O must have a present, unique `LC_UUID`. Measured fact (dual-group
clean-state probe, ADR 0018 2026-08-16 amendment): the Local Network alert
and its System Settings row are named `OpenCodeServerAgent` under BOTH
signing models — a platform limitation of the
LaunchAgent-launches-external-OpenCode-child architecture, not repairable
by signing identity, bundle naming, or purpose strings. Do not attribute
the prompt to a versioned Homebrew Cellar binary, and re-run the UI
attribution test only after a responsibility-chain change.

Every release candidate that changes bundle, helper, signing, or launch
structure MUST verify the TCC `AttributionChain` on a clean test state:

```text
responsible = ai.opencode.server
accessing   = /opt/homebrew/Cellar/opencode/<version>/bin/opencode
```

After granting FileProviderDomain once, replacing the Homebrew OpenCode child
with a different version/hash MUST NOT cause a new prompt. Failure of this test
blocks release.

## Configuration, credentials, and IPC

- Mutable configuration MUST live outside the signed app bundle.
- OpenCodeServer, OpenCodeServerAgent, and opencodeserverctl implement only the
  current IPC protocol and current persisted schemas. Do not add cross-version
  negotiation, fallback, migration, legacy-field cleanup, or compatibility
  layers while the product has no external users.
- The OpenCode password lives in the login keychain as a Generic Password
  item (service `ai.opencode.server`, account = effective username); see ADR
  0016. It MUST NOT be written to `config.plist`, the embedded LaunchAgent
  plist, or any other file. OpenCodeServer owns the item (create, in-place
  update, delete); OpenCodeServerAgent only reads it.
- Password changes MUST use `SecItemUpdate` in place — delete + re-add
  resets the whole ACL — but only when the value actually changed. After a
  real change the GUI MUST send the non-interactive `credential_changed`
  IPC notice; without it the agent keeps carrying the OLD password in
  memory and every "restart to apply" silently relaunches OpenCode with
  the stale credential. The notice flips the agent to `access_pending`
  (the running process keeps its old credential and stays supervised). The
  re-read then happens either through an explicit “Allow Keychain
  Access…” click or — when the persisted grant marker's recorded Team ID
  matches the running build's signing team — through one automatic silent
  re-read on the bounded worker; Save itself never requests consent. An
  unchanged save MUST stay a no-op (ADR 0016).
- Item creation MUST pre-seed the ACL with a `SecAccess` trusting the
  creating app and the embedded OpenCodeServerAgent
  (`SecTrustedApplication` by path, best-effort with a default-ACL
  fallback): a custom `kSecAttrAccess` replaces the default ACL on
  macOS 26, so omitting the creating app makes even its own read-back
  prompt, and the pre-seeded entry also collapses the two-stage consent
  into a single approval (mechanics: ADR 0016).
- Saving the first nonempty password or a real password change MUST remain
  non-interactive. If OpenCode is running, OpenCodeServer offers one
  contextual “Allow & Restart” action with “Later” as the alternative; only
  choosing the primary action may request Keychain consent, and the restart
  follows after OpenCodeServerAgent reports `configured`. If OpenCode is not
  running, do not show a restart alert; disclose the `Agent access` row and
  “Allow Keychain Access…” button in Settings instead.
- OpenCodeServerAgent routine Keychain work MUST use the attribute-only
  probe (`kSecReturnAttributes`), which can never raise UI. On macOS 26 the
  legacy-keychain consent dialog cannot be suppressed by any query key (ADR
  0016), so a decrypt-class read from a background path is allowed only
  when the persisted grant marker proves a decrypt already succeeded for
  that account with this build, or when the marker's recorded Team ID
  matches the running binary's own signing team — the latter authorizes ONE
  automatic silent read per account per process run, dispatched to the
  bounded worker. The deliberate interactive read still runs behind the
  Settings “Allow Keychain Access…” button, which remains the fallback for
  every unproven case. A missing grant is the soft `access_pending` state,
  never “not configured”, and never a reason to delete the item.
- The grant marker records the account, the bundle version, AND the signing
  Team ID of the build that last decrypted successfully (ADR 0016).
  Version-exact matches authorize the routine background read; team-exact
  (but version-mismatched, or post-`credential_changed`) matches authorize
  the single-shot automatic silent read; anything else — a fresh item, a
  team mismatch, a legacy two-line marker, an ad hoc build — stays on the
  explicit click. The measured basis (partition semantics across cdHash
  changes under self-signed vs Team-ID signing) and the scheduled
  revalidation points live in ADR 0016; revalidate before changing the
  minimum macOS, signing model, keychain implementation, or credential
  storage design.
- Background decrypt-class reads NEVER run inline on the supervisor event
  loop. They are dispatched to the single-flight bounded worker so a consent
  dialog (or any securityd latency) cannot stall process supervision or burn
  the SMAppService registration-transaction attempts during startup.
- The Rust agent reads the Keychain through the `security-framework` crate
  (safe API, OSStatus passthrough, no build-time code generation); this is
  the documented exception to the “small dependency set” default, justified
  in ADR 0016.
- Never log, print, test-snapshot, or expose the password in process listings.
- The menu never reveals the real password length; the password row appears
  only while authorization is pending (see “Menu bar scope”). Opening
  Settings performs only an off-main-thread, attribute-only Keychain probe;
  it MUST NOT decrypt the password or raise a consent dialog. The settings
  window may decrypt, reveal, or copy the password only after an explicit
  Edit or Copy action, and every Security.framework operation stays off the
  AppKit main thread. Existing credentials are represented as “Stored in
  Keychain”; removal is an explicit Remove-then-Save state, never inferred
  from an empty field.
- OpenCodeServerAgent/OpenCodeServer IPC should use a user-owned Unix domain
  socket in Application Support, mode `0600`.
- Authenticate local IPC peers with `getpeereid` or an equivalent supported
  same-user check.
- Keep the protocol small, versioned, bounded, and free of secrets in status
  responses.

## Menu bar scope

The status menu follows progressive disclosure (NN/g) and the HIG rule that
a menu presents a stable set of items: informational rows may appear and
disappear with state, but the action set never changes — actions are
disabled, never hidden. The authoritative row inventory (always-visible
rows, conditional rows and their triggers, the fixed action set, the
Settings window scope, and first-launch behavior) lives in
`PRODUCT_DECISIONS.md`.

OpenCodeServer MUST NOT configure or manage the macOS firewall.

The UX baselines (NN/g progressive disclosure, HIG Menus, HIG The Menu Bar,
HIG Onboarding, plus the full HIG snapshot) are stored outside the
repository at `~/Documents/OpenCodeServer-References/`; when a menu or
Settings change needs to cite them, point the reviewing agent at that path.

## Logging

- Use macOS Unified Logging for OpenCodeServer and OpenCodeServerAgent logs; do
  not build a separate text-log rotation subsystem.
- Use stable subsystem/categories and structured, privacy-conscious messages.
- Never log credentials, authorization headers, prompt content, or sensitive
  paths unless explicitly needed at a debug level and safely redacted.
- Record starts, stops, child exit reason, configuration validation failures,
  health transitions, and restart history for offline diagnosis.
- Do not put crash counts in the menu UI.

## Quality gates

No release candidate is complete until it passes:

- Rust formatting, linting, unit tests, and process-supervision integration
  tests;
- Swift formatting/static analysis and UI/state tests where practical;
- bundle layout validation;
- strict code-signature validation;
- Designated Requirement compatibility checks, with mutual satisfaction for
  same-identity upgrades and the accepted-ADR identity-migration carve-out
  described in the Code signing identity section;
- `SMAppService` registration, disablement, and re-registration tests;
- clean launch, graceful stop, forced stop, crash, and authorized process-group
  cleanup tests;
- authenticated health-check tests;
- confirmation that it listens only on the configured endpoint;
- VoiceOver labels, keyboard navigation, contrast, and non-color status tests;
- FDA functional-probe tests before and after user authorization;
- TCC AttributionChain tests;
- FileProviderDomain first-grant and Homebrew-upgrade persistence tests;
- rollback testing.

Use a clean VM or restorable test state for privacy-permission acceptance tests
when possible.

Re-run the SMAppService update path, FDA probe, TCC AttributionChain, and
FileProviderDomain persistence checks on clean state for every new macOS
release; the team-anchored Apple Development update path (ADR 0021) is a
per-OS acceptance gate, not a one-time certification.

## Project and change discipline

- Keep source modules small and responsibilities explicit.
- Prefer the simplest supported design.
- Do not add dependencies without documenting why the standard library or an
  Apple framework is insufficient.
- Preserve user-authored changes and unrelated work.
- Do not add migration or cleanup paths for obsolete development artifacts;
  current code reads and writes only the current schemas.
- Never include secrets in commits, patches, fixtures, screenshots, logs, or
  tool output.
- Update `PRODUCT_DECISIONS.md` and relevant design documentation when an
  accepted architectural decision changes.

## Public mirror discipline (dual-repository model)

Development happens in this private repository with full history. A public
mirror repository receives snapshot-only history through
`scripts/sync-public.sh`:

- The tracked tree must contain zero maintainer-identifying strings
  (signing identity names, email addresses, real names, Team IDs,
  certificate fingerprints or serials, absolute home-directory paths,
  machine hostnames). Record such facts with placeholders; concrete values
  live only in the maintainer-local runbook,
  `~/.config/opencodeserver/signing-identity`, and
  `~/.config/opencodeserver/sensitive-patterns`.
- Never push private branches or private history to the public remote;
  publishing goes only through `scripts/sync-public.sh`, whose
  sensitive-content gate reads the local patterns file and fails closed.
- Tracked maintainer-private documents (currently
  `docs/release-workflow.md`) are stripped from every snapshot by the
  script's `private_paths` exclusion list; new private documents must be
  added to that list, and the first sync after adding one should be
  `--dry-run` to confirm the exclusion notice.
- External contributions merge public → private
  (`git fetch public && git merge public/main`) and flow back to the public
  repository with the next snapshot.

## Local deployment workflow

Every completed source-code fix is treated as a local acceptance candidate:
after relevant tests pass, build a Release with a newer `CFBundleVersion`,
atomically install it, reopen it, and finish the installed runtime checks so
the user can immediately perform the real-machine test.

When a task produces a Release candidate intended for acceptance on this Mac:

1. Complete tests plus Bundle, signature, and version validation first.
2. Require the current ADR-designated Apple Development identity from the
   maintainer-local runbook, provided through
   `~/.config/opencodeserver/signing-identity` — enforced as the exact leaf
   authority by `scripts/install.sh` — never ad hoc signing, and require a
   `CFBundleVersion` newer than the installed build.
3. Send OpenCodeServer a normal quit request and wait for it to exit; never
   stop OpenCodeServerAgent or OpenCode.
4. Run `scripts/install.sh`, reopen `/Applications/OpenCodeServer.app`, and let
   `SMAppService` refresh the registered OpenCodeServerAgent.
5. Treat `register()` success as registration acceptance, not proof that
   OpenCodeServerAgent executed. Record `RegisteredBundleVersion` only after
   authenticated OpenCodeServerAgent IPC is reachable, then verify the
   installed OpenCodeServer and OpenCodeServerAgent bundle versions,
   OpenCodeServerAgent and OpenCode health, and the configured listening
   endpoint.
6. On failure, keep or restore the previous bundle and preserve diagnostic
   state. Do not ask the user to repeat these standard steps unless graceful
   quit fails, signing is incompatible, or broader authorization is required.

## Primary Apple references

The curated starting points for Apple documentation live in
`docs/apple-references.md`. Consult them first and follow their current
linked documentation.
