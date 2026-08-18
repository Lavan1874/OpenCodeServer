# Changelog

All notable user-visible changes to OpenCodeServer are recorded here.
Internal refactors, test additions, and documentation-only updates are
omitted unless they changed observable behavior.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/).

---

## Build 83 — 2026-08-17

### Changed
- The password caption is reworded as a state-neutral rule — "Without a
  password, OpenCode is unauthenticated." — replacing the imperative
  "Leave blank to keep…", which presupposed an empty field and read
  incorrectly while a password was stored.

## Build 82 — 2026-08-17

### Changed
- The Settings "Advanced" disclosure now presents as a Finder
  Get Info-style section header — a hairline above a full-width row with
  a chevron and a semibold title, clickable anywhere on the row —
  replacing the dated stock disclosure bezel (HIG disclosure-controls:
  a labeled disclosure triangle naming what it hides). VoiceOver now
  also hears the expanded/collapsed state.

## Build 81 — 2026-08-17

### Changed
- Settings drops its two permanent footer paragraphs. Facts that were
  buried there now arrive at their moment of need: the restart semantics
  of Save were already covered by the post-save feedback and the menu's
  configuration row; the Keychain authorization model now lives on help
  tags for Allow Keychain Access…, Copy, and the per-state Edit button.
- One short caption remains directly beneath the password field:
  "Leave blank to keep OpenCode unauthenticated." — the only fact worth
  permanent visibility, since it defines the field's semantics.

## Build 80 — 2026-08-17

### Changed
- The Settings password field now spans the full column width on its own
  row, with the Show/Edit/Copy/Remove controls on a second row beneath it,
  so long passwords no longer scroll inside a narrow field (HIG: size
  fields for their expected content; never scroll revealed text
  horizontally).
- The "Keep Saved" button shown while editing an existing password is
  renamed "Cancel Edit" — its behavior is unchanged (the stored Keychain
  item stays untouched), the new title says what it actually does.

## Build 78 — 2026-08-17

### Changed
- Same-team upgrades no longer require the "Allow Keychain Access…" click:
  when the persisted grant marker's recorded Team ID matches the running
  build's signing team, OpenCodeServerAgent performs one automatic silent
  Keychain re-read on its bounded worker (measured silent on macOS 26.6.1
  for same-team cdHash changes, password changes, and agent restarts — ADR
  0016, 2026-08-17 amendments). A fresh install's first grant, a team
  mismatch, and ad hoc builds still use the explicit click; a failed
  automatic read falls back to it as well.
- Real password changes are re-applied silently in the background under the
  same team rule; the "Allow & Restart" click now only decides whether
  OpenCode restarts immediately. The restart choice itself stays manual.
- Because markers written by earlier builds carry no Team ID, this upgrade
  requires one last manual grant; upgrades from Build 78 onward are
  click-free.

### Fixed
- Reading the signing team from the process's own code signature requires
  `kSecCSSigningInformation`: measured on macOS 26.6.1,
  `SecCodeCopySigningInformation` with default flags omits
  `kSecCodeInfoTeamIdentifier` from the result dictionary.

## Build 76 — 2026-08-16

### Changed
- Destructive confirmation alerts (Force Stop, Stop/Restart/Repair
  interruptions) now default the Return key to Cancel, so a stray Return can
  no longer trigger a destructive action.
- The menu bar status indicator now uses a distinct SF Symbol per state
  (checkmark / warning triangle / octagon / gray circle) in addition to
  color, so status no longer relies on hue alone.

### Fixed
- OpenCodeServerAgent no longer crashes and restarts when a reattached
  OpenCode exits and is reaped by launchd in the narrow window before the
  supervisor's kqueue watch is installed; the exit is now classified through
  the normal poll path.
- The agent caps simultaneous status-subscription connections (4) so a
  misbehaving same-user client cannot exhaust its file descriptors.
- IPC connections to OpenCodeServerAgent now have a bounded connect timeout
  instead of a potentially indefinite blocking connect.
- A late one-shot status poll can no longer briefly overwrite a newer pushed
  status in the menu.
- Settings validation now rejects control characters and malformed
  hostnames up front, matching the agent's authoritative checks, and
  loopback detection covers non-canonical IPv6/IPv4 forms.
- SIGHUP now triggers an explicit configuration recheck instead of running a
  discarded validation.
- Bracketed IPv6 hostnames (for example `[::1]`) are normalized at
  validation time so spawning, health checks, and endpoint display all use
  one canonical form.

### Internal
- `CFBundleVersion` moved to `Config/Base.xcconfig` as the single source of
  truth.
- The Rust file lists driving the Xcode build phase were regenerated for the
  ADR 0019/0016/0018 module layout.

## Build 75 — 2026-08-16

### Changed
- Release acceptance builds now use the Apple Development identity
  `Apple Development: <developer-account> (<leaf-id>)` with the personal Team
  ID, while Debug, test, and CI builds retain their ad hoc signing override.
- The Build 74 identity migration is followed by a same-identity Build 75
  upgrade path using mutual Designated Requirement validation; the migration
  and its one-time Keychain authorization are recorded in the ADR 0021 test
  records.

## Build 74 — 2026-08-16

### Changed
- Release signing identity migrated from the retired self-signed
  `OpenCodeServer Local Signing` certificate to the Apple Development identity
  `Apple Development: <developer-account> (<leaf-id>)` (Team ID
  `<team-id>`), per ADR 0021. The two Designated Requirements do not satisfy
  each other; the one-way transition evidence is recorded in the ADR 0021
  test records, and the installer gained an explicit identity-migration gate
  for it. Debug, test, and CI builds remain ad hoc.
- As predicted by ADR 0021, the first Apple-signed install produced exactly
  one stale-cdHash "OpenCodeServerAgent quit unexpectedly" dialog, which the
  bounded registration transaction self-healed in about 15 seconds; the
  same-identity Build 75 upgrade then produced no crash dialog.
- The signing-model change requires one explicit Keychain re-consent (ADR
  0016/ADR 0021): OpenCodeServerAgent reports the credential as
  access-pending until the user approves Settings → "Allow Keychain Access…"
  once, then recovers without a password change.

## Build 72 — 2026-08-13

### Fixed
- Script wrappers in `PATH` (e.g. a `~/bin/opencode` shell shim) no longer
  appear as Settings executable candidates. The Swift candidate filter now
  requires an arm64 Mach-O binary, matching the Rust agent's validation.

## Build 71 — 2026-08-13

### Fixed
- A hostname resolving to multiple addresses where the first is a black
  hole no longer stalls the supervision loop for up to 16 seconds. Health
  checks now share one total deadline across all resolved addresses.
- Restarting OpenCode while a previous port-release retry is still active
  now receives a full retry budget instead of inheriting a partially
  expired deadline.

## Build 70 — 2026-08-13

### Fixed
- Unauthenticated IPv6 loopback endpoints (`[::1]:port`) are now recognized
  as loopback and no longer trigger a spurious "Authentication: Not enabled"
  warning row in the menu.

### Removed
- `KeychainStore.save`, an unused helper that decrypted the password to
  compare values, has been deleted. Its only test caller now exercises the
  same validation through `create`.

## Build 69 — 2026-08-13

### Fixed
- The "Restart OpenCode…" menu action is now disabled while Keychain
  access is pending after a password change, preventing an unintended
  service stop that could strand OpenCode in the Failed state.

## Build 68 — 2026-08-12

### Fixed
- Every IPC response now carries a non-empty `AgentStatus`, including
  `validate_config` failure reports. One-shot and subscription validation
  are unified through a single `requireCurrentStatus` check.
- The one-shot IPC channel requires a terminating line feed before parsing.
- `OPENCODESERVER_SUPPORT_DIR` is resolved identically in Rust and Swift:
  absent/empty/whitespace falls back to Application Support, relative paths
  are rejected, absolute paths are used verbatim.

## Build 67 — 2026-08-12

### Fixed
- IPC handshake reads and writes are bounded by a shared 5-second absolute
  deadline per connection. A slow reader can no longer indefinitely block
  a supervision-loop slot; connections that exceed the deadline are
  released and retried.
- Credential IPC notices (`.created`, `.changed`, `.removed`) are now
  delivered reliably through a persistent journal with serial delivery and
  agent acknowledgment, replacing the previous fire-and-forget path.

## Build 66 — 2026-08-12

### Changed
- Local Network privacy attribution documented as a measured macOS 26
  platform limitation under the current self-signed, no-Team-ID signing
  model. The SMAppService responsibility chain and LC_UUID prerequisites
  are verified; fallback attribution is a known limitation, not a defect.

## Build 64 — 2026-08-11

### Fixed
- Local Network privacy attribution metadata corrected for the helper
  bundle. Credential removal now clears the grant marker consistently.
- Notification event IDs use UUIDv4 for global uniqueness.

## Build 56 — 2026-08-09

### Fixed
- Saving an unchanged password gives honest feedback instead of implying
  a grant-revoking write occurred.

## Build 55 — 2026-08-09

### Added
- OpenCode password is now stored as a login-keychain Generic Password
  item (`ai.opencode.server`, account = username), replacing the plaintext
  `Password` key in `config.plist`. OpenCodeServer owns create/update/delete;
  OpenCodeServerAgent only reads.
- Routine Keychain access uses an attribute-only probe that cannot raise UI.
  Background decrypts are gated by a persisted grant marker bound to both
  the account and the agent's bundle version. The one interactive read runs
  behind the explicit Settings "Allow Keychain Access…" button.
- Settings "Allow & Restart" action: a password-changing Save offers one
  contextual dialog that raises the system consent prompt and restarts
  OpenCode automatically once the grant lands.

### Changed
- Menu redesigned around progressive disclosure (NN/g) and HIG "stable
  action set": steady state shows only health, uptime, endpoint, and
  version. Agent/FDA/password/authentication/configuration rows appear
  only while they deviate from nominal. Actions are disabled, never hidden.
- Rarely used actions (Open Logs, Recheck FDA, Open FDA Settings, Open
  Login Items Settings, Repair OpenCodeServerAgent…) moved into a
  single-level Advanced submenu.
- Settings hides mDNS and executable selection behind an Advanced disclosure
  that auto-expands for non-default loaded values.
- Username removed from the menu; shown in Settings only.
- First launch opens the Settings window exactly once.

### Fixed
- An identity-verified but configuration-stale OpenCode process is now
  taken over as managed (`config_pending`, stoppable/restartable) instead
  of being abandoned as an unrecoverable fault.

## Build 38 — 2026-08-06

### Changed
- Uptime display always shows seconds, making the per-second menu tick
  visibly verifiable past the first minute.

## Build 37 — 2026-08-06

### Fixed
- Menu items no longer flicker between enabled and disabled on every menu
  open. Automatic item validation is disabled; the status update owns all
  item availability.
- Timers (uptime, fallback polling, registration verification) continue
  firing while the menu is open by scheduling in common run loop modes.
- "Open Login Items Settings" label aligned with HIG (ellipsis removed
  for non-dialog actions).

## Build 36 — 2026-08-06

### Fixed
- Adaptive IPC verification window for the stale launch-constraint cascade:
  the GUI polls status once per second while registration verification is
  pending, instead of waiting for a single deferred push.

## Build 26 — 2026-08-05

### Fixed
- Installed-version query boundary simplified: clean queries are not
  signaled on shutdown.

## Build 24 — 2026-08-05

### Fixed
- Observed query group escape now fails closed instead of proceeding.

## Build 23 — 2026-08-05

### Fixed
- Installed-version queries are leased across agent death, preventing
  orphaned child processes.

## Build 22 — 2026-08-05

### Changed
- Audited query ownership: the supervisor owns installed-version query
  cleanup through agent shutdown.

## Build 21 — 2026-08-05

### Fixed
- Unverified process convergence is durable across agent restarts.
- Escaped-group agent restart lifecycle validated.

## Build 20 — 2026-08-05

### Fixed
- Post-spawn survivor types and unverified convergence closed.
- SIGKILL after reap eliminated from the installed-version query.
- Platform layer and test-only API boundaries tightened.

## Build 19 — 2026-08-05

### Fixed
- Single Rust unsafe boundary restored in platform layer.
- Post-spawn ownership lifecycle closed.
- Installed-version query given a complete resource deadline covering
  descendants holding stdout.
- Default-parallel test suite made deterministic.

## Build 18 — 2026-08-05

### Fixed
- Upgrades and process reattachment hardened.
- IPC client error handling and test recovery improved.
- Service management and runtime state refined.

## Build 16 — 2026-08-05

### Fixed
- Process identity is kept when the executable file is replaced (Homebrew
  upgrade), preventing false identity-mismatch failures.

## Build 15 — 2026-08-05

### Fixed
- Startup waits for the endpoint address to be available before declaring
  healthy, preventing premature health transitions.

## Build 14 — 2026-08-04

### Changed
- OpenCodeServerAgent runs as `ProcessType Interactive`, resolving
  launchd launch-constraint violations on macOS 26.

## Build 13 — 2026-08-04

Initial recorded acceptance baseline.
