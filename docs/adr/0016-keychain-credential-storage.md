# ADR 0016: OpenCode password moves from config.plist to the login keychain

## Status

Accepted, 2026-08-04. Scope clarified from isolated validation, 2026-08-09.
Team-signing observations recorded, 2026-08-17 (amendment below). Product
rule relaxed for same-team transitions by the second 2026-08-17 amendment;
the ADR 0021 phase-5 reissue experiment remains the next revalidation point.

## Context

Through Build 38 the OpenCode password lived in the user-owned
`config.plist` (mode 0600) under Application Support. That satisfied the
minimum bar — the secret never sat inside the signed bundle — but a
plaintext-at-rest credential in a regular file is not the macOS-idiomatic
answer, and Apple documentation and platform practice both point at the
Keychain for exactly this class of secret.

The migration changes where the secret lives and, necessarily, who may read
it and how access is granted:

- OpenCodeServer (the GUI) creates, updates, and deletes the item.
- OpenCodeServerAgent (a nested, separately signed Mach-O started by
  launchd) only reads it, and per TN3137 it is a **separate Keychain
  subject** from its host app: the first read by the agent triggers the
  system authorization prompt unless the user grants "Always Allow".

## Platform facts

- **File-based keychain semantics.** The login keychain is a file-based
  keychain. Per
  [TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains),
  `kSecAttrAccessible*` values are effectively a no-op there (the access
  control model is "completely different"); the behavior we want — unlocked
  after login, readable while the screen is locked, never synced — is the
  login keychain's inherent behavior. OpenCodeServer still writes
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` at creation time as
  documented intent for a future data-protection-keychain migration.
- **No data-protection keychain.** Setting `kSecAttrSynchronizable` or
  otherwise routing the item to the data-protection keychain would make it
  unreadable for a launchd-started background process without a Team ID and
  keychain-access-group entitlements (`errSecMissingEntitlement` -34018).
  Neither attribute is ever set.
- **The application ACL and XARA partition are distinct authorization
  layers.** "Always Allow" can add the calling application to the item's
  application ACL with trust anchored on its Designated Requirement, but
  compatibility with that requirement is not, by itself, proof of silent
  decrypt access to the login keychain. The isolated 2026-08-09 experiment
  described below held the path and mutually compatible Designated
  Requirements constant while changing the reader cdHash; the login keychain
  still required consent because its separate `partition_id` did not contain
  the new cdHash.
- **`SecItemUpdate` preserves the ACL application list; delete+add resets
  the whole ACL.** Re-adding an item recreates it with the creating
  process as the only trusted application, which would force the agent
  through the authorization prompt after every password change (observed
  in the wild, e.g. CodexBar issue #340). Password changes therefore
  always update `kSecValueData` in place — but see the next-but-one fact:
  on macOS 26 the update still wipes the `partition_id` grant, so it is
  reserved for real changes and unchanged saves are no-ops. A username
  change is a new `account` and thus unavoidably a new item with a fresh
  ACL; the Settings UI says so explicitly.
- **`SecItemCopyMatching` blocks the calling thread** with no documented
  timeout when user interaction is possible, and — measured on macOS 26
  (2026-08-04, signed probe binaries against throwaway items) — **the
  legacy-keychain consent dialog can no longer be suppressed at all**:
  `kSecUseAuthenticationUISkip`, `kSecUseAuthenticationUIFail` (deprecated
  since macOS 11), and the officially recommended
  `kSecUseAuthenticationContext` + `LAContext.interactionNotAllowed` all
  raised the dialog anyway (SecurityAgent observed on screen; each probe
  blocked ~6–9 s until answered). The `SecItem.h` note on
  `kSecUseNoAuthenticationUI` states the platform direction outright:
  "Legacy keychain items will still activate UI if needed." Pre-seeding the
  ACL (`security -T`, i.e. the `SecAccess` path) does not help either: it
  writes the application list but not the `partition_id` list, and only
  interactive consent adds the caller there — the first read still prompts.
  One consequence makes the design workable: **attribute-only reads
  (`kSecReturnAttributes`, no `kSecReturnData`) are not ACL-gated and never
  prompt**. An earlier throwaway-item probe appeared to show that one "Always
  Allow" survived a later rebuild with a new cdHash. The isolated 2026-08-09
  experiment explains why that observation cannot be generalized: its newly
  created temporary file keychains used legacy version-256 behavior and did
  permit silent reads after replacement, whereas the login keychain used
  version-512 partition enforcement and prompted for the new cdHash.
- **macOS 26 consent is two-stage for a fresh item** (measured 2026-08-05
  during the v41 walkthrough, securityd `kcacl`/`integrity` logs). The
  first "Always Allow" that CREATES the caller's ACL application entry does
  not add the caller to the item's `partition_id` list; the next agent
  process then fails the partition check (`ACL partition mismatch`) and
  prompts again, and only that second approval writes the partition entry.
  When the approving binary ALREADY has an ACL application entry at consent
  time (e.g. one pre-seeded with `SecAccess`), the first approval writes
  both stages. The product therefore pre-lists OpenCodeServerAgent in the
  item ACL at creation: not to avoid the consent dialog (impossible), but
  to make one "Always Allow" sufficient.
- **macOS 26 `SecItemUpdate` wipes the `partition_id` list** (measured
  2026-08-05 during the v42 walkthrough, securityd `integrity` logs plus
  item `cdat`/`mdat` forensics). An in-place update keeps the ACL
  application entries, but the partition grant written by an earlier
  "Always Allow" is gone afterwards, so the next agent PROCESS prompts
  again even though the approving process kept working from securityd's
  cache. Consequence: any real password change silently revokes the
  agent's grant; a redundant save with an unchanged value must therefore
  be a no-op (no `SecItemUpdate`) so it cannot revoke the grant. Note the
  grant marker still covers the value in securityd's cache for the rest
  of the current process, which is why the revocation only surfaces at
  the next agent restart.
- **The current product's login-keychain partition grant is cdHash-specific.**
  The 2026-08-05 v42→v43 OpenCodeServer atomic-replacement walkthrough first
  observed this in the product's `securityd` `integrity` logs. The isolated
  2026-08-09 KeychainPartitionProbe experiment then controlled the path,
  signing identity and Designated Requirement independently on macOS 26.5.1
  (25F80): its login keychain was version 512; Reader A and Reader B had
  mutually compatible Designated Requirements and different cdHashes; Reader
  B prompted after the A→B same-path atomic replacement; and "Always Allow"
  added Reader B's cdHash to `partition_id`, after which a fresh Reader B read
  silently. This directly supports treating a different-cdHash replacement as
  unproven for OpenCodeServer's present self-signed/no-Team-ID login-keychain
  configuration. It does **not** establish a universal rule for every
  file-based keychain: the same experiment's newly created version-256
  temporary keychain allowed Reader B to read silently. Developer ID/Team ID,
  incompatible-signature, byte-identical replacement, and launchd-specific
  controls were outside that experiment's scope. Consequence for the current
  product: a new bundle version cannot inherit the marker's silent-read
  evidence; OpenCodeServerAgent reports `access_pending` and waits for one
  user-initiated Settings "Allow Keychain Access…" action. Revalidate before
  changing the OS baseline, signing model, keychain implementation, or storage
  design.
- **Error taxonomy.** `errSecItemNotFound` (-25300) is the only reliable
  "no password configured" signal. `-25308`, `errSecUserCanceled` (-128),
  `errSecAuthFailed` (-25293), and -34018 all mean "cannot read right now"
  and are treated as a soft *access pending* state: the agent never deletes,
  never reports "not configured", and never auto-prompts because of them.

## Decision

1. **Storage.** The password is a Generic Password item in the login
   keychain: `kSecAttrService = "ai.opencode.server"`,
   `kSecAttrAccount = <effective OpenCode username>` (blank usernames fall
   back to `opencode`, matching the agent's `effective_username`).
2. **Current schema only.** `config.plist` has no password field. Neither
   component detects, erases, ignores specially, or migrates historical
   plaintext fields. An empty Keychain credential keeps OpenCode's native
   unauthenticated behavior, which remains a supported user choice.
3. **Ownership split.** OpenCodeServer writes (add / in-place update /
   delete); new items are created with a `SecAccess` that pre-lists the
   creating app AND the embedded OpenCodeServerAgent (legacy
   `SecTrustedApplication` by path — `NULL` for self — which securityd
   stores as an `identifier + certificate root` requirement, stable
   across rebuilds). The creating app must be listed explicitly: a custom
   `kSecAttrAccess` replaces the default ACL on macOS 26, so an item
   created with only the agent in the list prompts even on the GUI's own
   read-back (measured 2026-08-05; blocked hosted XCTest). Pre-seeding
   cannot remove the one consent dialog — on macOS 26 only interactive
   approval writes the partition grant — but it makes that single
   approval complete (see the two-stage consent platform fact).
   OpenCodeServerAgent reads only, merging the credential into its
   in-memory configuration so the configuration fingerprint, the spawn
   environment, and the health-check authenticator keep one source.
4. **Grant UX.** Routine agent work (startup, kqueue reload, periodic
   recheck) performs only the attribute-only `probe_item`, which cannot
   raise UI; a decrypt-class read from a background path is dispatched
   solely when the persisted **grant marker** (Application Support
   `credential-grant`, content: the account name AND the agent's bundle
   version) proves a decrypt already succeeded for that account with this
   exact build. Under the currently validated login-keychain and signing
   configuration, the version scope conservatively refuses to treat an
   untested replacement binary as already authorized: the post-upgrade agent
   probes, reports `access_pending`, and waits for the user's one deliberate
   consent instead of raising a background dialog. The marker
   is written after every successful decrypt and cleared when the item
   vanishes, a decrypt reports access-pending, or the GUI's
   `credential_changed` notice reports a rewrite (the SecItemUpdate
   invalidated the partition grant, so the recorded evidence is spent —
   keeping it would let the next agent start raise a background prompt
   ahead of the explicit Allow Keychain Access flow), so a revoked grant
   cannot re-prompt on every reload. The marker parser accepts only the
   current two-line account-and-bundle-version schema. NO decrypt ever runs inline on the
   event loop: marker-permitted reads use the same single-flight worker
   as the interactive flow, because a wrong expectation (grant revoked
   in Keychain Access) would block the loop behind SecurityAgent and
   stall IPC — measured 2026-08-05, an inline startup read burned all
   three attempts of the Service Management registration transaction.
   (The marker schema and the `credential_changed` clearing described in
   this item are superseded by the second 2026-08-17 amendment for
   team-signed builds: the marker is now the three-line
   account + bundle-version + Team-ID schema, a team-matching marker
   authorizes one automatic silent re-read, and `credential_changed` no
   longer retires a team-matching marker. The two-line behavior described
   here remains the recorded basis for every no-team-evidence fallback.)
   The Settings window shows an
   `Agent access` row (Granted / Not granted / — / Unknown) with a
   **Allow Keychain Access…** button that sends the `refresh_credentials` IPC
   command; that command performs OpenCodeServerAgent's one deliberate
   decrypt-class read on a dedicated worker thread and is its only code path
   expected to raise the consent dialog. A granted read also resumes a start that was
   previously refused. The menu password row points to Settings while access
   is pending, and one notification per pending episode backs both up.
   Because a real password change wipes the partition grant (platform fact
   above), a Save that updated an existing item sends the non-interactive
   `credential_changed` notice. An explicit deletion sends the same notice so
   OpenCodeServerAgent cannot keep carrying the removed password. OpenCodeServerAgent
   clears the marker, rejects the carried-over old password, and reports
   `access_pending` after a creation or update, or `not_configured` after
   deletion. Creating the first item and updating an existing item both use the
   same reliable `credential_changed` notification path: OpenCodeServer persists
   a bounded pending mutation, serializes its IPC delivery, retries it after IPC
   reconnection, and clears it only after OpenCodeServerAgent acknowledges the
   corresponding credential state. This supersedes the original decision that a
   creation needed no notice because there was no old in-memory password to
   invalidate. Finding 2 showed a fail-open timing window in that reasoning:
   before the periodic attribute probe ran, OpenCodeServerAgent still reported
   `not_configured`, and the fail-closed spawn gate only rejected
   `access_pending`. An immediate Start could therefore launch OpenCode without
   authentication even though the new Keychain item already existed. The
   creation notice changes `not_configured` directly to `access_pending`, making
   the existing fail-closed gate effective without waiting for periodic review.
   Save itself never asks Keychain to decrypt and therefore never raises the
   consent prompt. If
   OpenCode is running, either creation or update offers one contextual
   “Allow & Restart” dialog. Only the user's explicit click sends
   `refresh_credentials`; after a successful `configured` status push,
   OpenCodeServer restarts OpenCode so the new password takes effect. “Later”
   preserves the running work. If OpenCode is stopped, no restart alert is
   shown; Settings discloses the `Agent access` row and
   “Allow Keychain Access…” button instead. An unchanged save stays a no-op
   end to end.
   Opening Settings is also non-interactive: OpenCodeServer performs an
   attribute-only existence probe on a worker queue and represents an existing
   value as “Stored in Keychain” without fetching it. Only explicit `Edit…`
   and `Copy` actions perform a GUI decrypt-class read; those reads also use a
   worker queue because `SecItemCopyMatching` can wait indefinitely for the
   system dialog. `Show` is available only after Edit has loaded the value.
   Remove is an explicit pending mutation committed by Save, never inferred
   from an empty field. Save compares an edited value with the already loaded
   original and performs all add/update/delete work on a worker queue, so it
   neither decrypts for comparison nor blocks AppKit's main thread.
5. **Fail-closed spawn gate.** If a password may exist but the agent is not
   authorized to read it (`AccessPending`), starting OpenCode is refused
   with an actionable error instead of silently spawning without
   authentication.
6. **401 guidance.** A health-check HTTP 401 is classified separately
   (`HealthError::Unauthorized`) and produces dedicated `last_error` and
   notification text: re-save the password in Settings and restart. If the
   item was removed while OpenCode still runs with the spawned password, the
   message says the credential was removed from Keychain.
7. **Protocol.** `password_configured: bool` becomes
   `password_state` (`not_configured` / `access_pending` / `configured`);
   `PROTOCOL_VERSION` was 3 when `Command::RefreshCredentials` was added;
   Build 63 raised it to 4 for the current-only `credential_removed` command,
   and Build 64 raised it to 5 for ADR 0017's UUID notification `event_id`.
   The action-capability status schema later raised it to 6. OpenCodeServer,
   OpenCodeServerAgent, and opencodeserverctl ship in one bundle and now accept
   only protocol 6, so no cross-version compatibility is maintained.
8. **Rust dependency.** The agent reads the Keychain through
   `security-framework` 3.7 (`default-features = false`): a safe API over
   `SecItemCopyMatching` that passes OSStatus codes through untouched, with
   no cc/bindgen step, and the same crate rust-lang/cargo uses for its
   macOS credential helper. The crate carries a "looking for maintainer"
   badge but releases regularly; the surface used here is two functions and
   is isolated in `rust/src/keychain.rs`, so replacement is cheap if
   maintenance lapses. `errSecUserCanceled` and
   `errSecInteractionNotAllowed` are not exported by
   `security-framework-sys` 3.x and are defined locally from Apple's public
   `SecBase.h` values.
9. **Tests.** Ad-hoc signed test binaries would prompt on real Keychain
   reads, so fixture builds (`test-fixture` feature / `cfg(test)`) read the
   credential from an `OPENCODESERVER_TEST_PASSWORD` environment hook that
   never compiles into production builds.

## Consequences

- The plaintext password no longer exists at rest anywhere in the product's
  writable state; `config.plist` loses its most sensitive field and its
  0600 discipline now protects only the configuration fingerprint key and
  runtime state.
- A current-schema install starts without a credential. The user creates one
  in Settings and explicitly grants OpenCodeServerAgent access when needed;
  there is no upgrade-time plaintext-password migration flow.
- A bundle replacement never reuses the previous build's grant-marker
  evidence. The current build reports `access_pending` until the user invokes
  “Allow Keychain Access…”; this is current product behavior and is validated
  separately from the still-required clean-state platform acceptance tests.
- The agent's credential state machine (`NotConfigured` / `AccessPending` /
  `Available`) is deliberately small and mirrors the version-query
  single-flight pattern; no retry circuit breaker. The only persisted piece
  is the grant marker file, and losing it is fail-safe (one more explicit
  "Allow Keychain Access…" click). Credential state transitions are logged at Notice
  level — the 2026-08-04 prompt-storm incident (v39 routine "skip-UI" reads
  raising the unsuppressible macOS 26 consent dialog on every config reload)
  showed that silent credential states make field diagnosis impossible.

## Exit route

The file-based keychain is, per TN3137, "on the road to deprecation". If
OpenCodeServer later obtains a Developer ID / Team ID, the migration path
is the data-protection keychain with a shared keychain-access-group
entitlement for both binaries (`kSecUseDataProtectionKeychain`), which
removes the ACL prompt entirely. Until then the design above is the
supported mechanism. In the tested macOS 26.5.1 login-keychain configuration,
pre-seeded trust lists (`SecAccess`) cannot make the first read silent — only
interactive consent writes the partition grant — but they are still used at
item creation, because a pre-seeded entry turns the first "Always Allow" into
a complete grant and spares the user the second prompt.

## Amendment — 2026-08-17: Team-ID silent-grant observations

The ADR 0021 migration to the Apple Development (Team ID `<team-id>`)
identity activated this ADR's revalidation clause: the cdHash-specific
partition grant above was measured self-signed/no-Team-ID, and Team-ID
configurations were explicitly outside the 2026-08-09 experiment's scope.
The first same-team upgrade of the shipped product (Build 75 → Build 76,
2026-08-17, macOS 26.6.1, version-512 login keychain) supplied the first
real-product evidence, captured in Unified Logging on the designated build
Mac (all times 2026-08-17):

1. **Same-team cdHash change no longer prompts (agent).** At 06:21:24 the
   freshly installed Build 76 OpenCodeServerAgent (new cdHash) performed the
   deliberate interactive read behind "Allow Keychain Access…" and went
   `AccessPending → Available` in ~0.2 s with **no securityd prompt event**.
   Under the self-signed measurements (2026-08-05 walkthrough, 2026-08-09
   probe) this exact transition always prompted. The partition grant is
   team-anchored in practice: the pre-seeded ACL requirement cites
   `identifier … and anchor apple generic and certificate leaf[subject.CN] =
   "Apple Development: …"` plus the Team ID extension — satisfied by any
   same-team build regardless of cdHash.
2. **Grants remain per code identity, not per team.** At 06:26:26 securityd
   displayed the consent prompt for the **GUI** process on its first-ever
   decrypt-class read of the same item (Edit/Copy path); the user approved
   "Always Allow". The agent's grant never covered the GUI and vice versa.
   Team anchoring makes each identity's own grant survive cdHash changes; it
   does not merge the two identities' grants. After this one-time approval,
   both identities hold team-anchored grants.
3. **A real `SecItemUpdate` password change no longer revokes in practice.**
   At 06:55:22 the password was changed via Settings (in-place
   `SecItemUpdate`); the agent entered `access_pending` by design, and the
   single "Allow & Restart" click re-read **silently**. At 06:56:04 a
   **fresh** OpenCodeServerAgent process (new pid) read the changed password
   silently at startup under the existing grant marker. A second change
   round at 06:56:20/24 reproduced the silent re-read. No securityd prompt
   appears anywhere in the window. Under the 2026-08-05 self-signed
   measurement, the post-update read prompted again. Whether `SecItemUpdate`
   still wipes `partition_id` but the wipe no longer bites for same-team
   processes, or the wipe does not occur for team-signed items, is
   undetermined — the observable product behavior is silent in all rounds.

**Product rule at the time of these observations.** When written, the
grant marker remained bound to account + bundle version and the click gates
("Allow Keychain Access…", "Allow & Restart") were the only paths that could
raise a consent dialog. The second 2026-08-17 amendment below relaxes this
for same-team transitions on the strength of these observations. The
remaining boundaries to revalidate: certificate reissue (the pre-seeded
requirement cites the exact leaf CN suffix `(<leaf-id>)` — ADR 0021 phase
5, due 2026-08-28), clean-state fresh install, and per-OS revalidation.

Note on the exit route above: the product obtained a Team ID via ADR 0021
without moving to the data-protection keychain. That route remains open;
these observations merely record that the file-based login keychain's
practical friction dropped substantially under team-anchored signing.

## Amendment — 2026-08-17 (product rule): same-team automatic silent read

Following the observations above, the click gate is relaxed for exactly the
transitions those observations measured:

- The grant marker now records **account + bundle version + Team ID**
  (three-line schema; legacy two-line markers are absent evidence, so the
  upgrade that introduces this rule needs one last manual grant).
- When the marker does not cover the running build's version but its
  recorded Team ID matches the running binary's own signing team (read from
  its own code signature at runtime), OpenCodeServerAgent dispatches **one
  automatic decrypt-class read on the bounded worker** — at startup/config
  merge, and on a `credential_changed` notice (in which case the marker is
  kept, not retired, and the new password is re-applied silently). Success
  records a fresh marker for the new version; any failure falls back to
  today's `access_pending` + explicit click behavior.
- The automatic dispatch is **single-shot per account per process run**;
  a transient failure never re-arms it, so a wrong expectation can raise at
  most one background consent dialog instead of one per recheck.
- Everything with no prior grant history stays manual: a fresh item's
  first-ever grant, any team mismatch (identity change, ad hoc/self-signed
  builds), and the GUI's explicit Edit/Copy decrypt.
- The **restart decision stays manual**: after a password change the user
  still chooses Restart/Later; only the authorization read became
  automatic.

Accepted risk, stated honestly: if a future transition (certificate
reissue, OS behavior change) turns out NOT to be covered by the
team-anchored grant, the automatic read can raise one unsuppressible
consent dialog from the background agent — the same dialog the click would
have produced, minus the click context. The ADR 0021 phase-5 reissue
experiment (2026-08-28) remains the next revalidation point; if it shows
the grant does not survive reissue, this rule narrows back.

Implementation note (measured, macOS 26.6.1): reading the running
process's own team via `SecCodeCopySelf` +
`SecCodeCopySigningInformation` only returns `kSecCodeInfoTeamIdentifier`
when the `kSecCSSigningInformation` flag is passed; with default flags the
key is absent, which this design reads as "no team" (fail-safe, never a
guess).

## Amendment — 2026-08-17: username-change save transaction and migration journal

Changing the OpenCode username moves the credential to a different Keychain
account. That save is an explicit multi-phase transaction instead of a
create-then-delete sequence (2026-08-17 high-priority review finding):

- Ordering is fixed: create the new account's item → save `config.plist` →
  delete the old account's item. The old item is untouched until the new
  configuration is durable, and a failed old-item deletion is non-blocking:
  the save succeeds with a warning and the cleanup is retried.
- A durable migration journal (accounts and generation only, never the
  credential or derived values) records five phases: `staged` →
  `newCredentialReady` → `configurationSaved` → `cleanupOld` → complete.
  Journal writes use the same file-fsync/rename/directory-fsync discipline
  as the Rust runtime state, with an explicit uncertainty outcome that keeps
  the in-memory state aligned with the possibly-visible file.
- Crash recovery reconciles the journal against the persisted configuration
  and attribute-only Keychain probes: configuration still names the old
  account → the new item is inactive and is removed; configuration names
  the new account and the item exists → the migration completes and the old
  item becomes the cleanup candidate; anything ambiguous holds the intent
  for a later retry. Every deletion re-reads the current configuration and
  re-confirms the expected account immediately before it runs.
- Recovery never decrypts and never prompts: probes are attribute-only and
  deletions run with `kSecUseAuthenticationUIFail`, so an authorization
  requirement retains the intent instead of raising a dialog.
- Same-account create/update/delete keeps the simpler configuration-first
  transaction; a username change combined with pending password removal is
  rejected in the UI rather than guessed.

Locked by `CredentialMigrationTests.swift` (transaction, journal-failure,
and recovery-decision suites).

## References

- [TN3137: On Mac keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
- [SecItemCopyMatching](https://developer.apple.com/documentation/security/secitemcopymatching(_:_:))
- [SecItemUpdate](https://developer.apple.com/documentation/security/secitemupdate(_:_:))
- [Keychain items: sharing and access control](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps)
- Apple, *If you see a "... wants to access your keychain" alert* (user
  documentation; "updated apps must sometimes reauthorize")
- KeychainPartitionProbe isolated experiment, 2026-08-09: macOS 26.5.1
  (25F80), self-signed/no-Team-ID A/B readers, mutually compatible Designated
  Requirements, same-path atomic replacement, version-512 login keychain and
  version-256 temporary-keychain controls. The experiment project retains the
  complete procedure, logs, raw evidence, and final report outside this
  repository at `~/Projects/KeychainPartitionProbe`.
- rust-lang/cargo `macos/keychain.rs` (same crate, same read pattern)
