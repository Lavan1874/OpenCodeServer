# ADR 0021: Signing identity migration to Apple Development (team-anchored code signing)

Date: 2026-08-16

## Status

Accepted, including a deliberate expiry-experiment rider (Decision §3).
Implemented and merged to `main` (merge `55f472b`, 2026-08-16): Build 74
is the migration build and Build 75 the same-identity upgrade; phases 0–4
are measured in the post-implementation sections below and
`docs/test-records/adr0021-*-20260816.md`. The phase-5 expiry experiment
remains pending the certificate's 2026-08-28 expiry.

Amendment — 2026-08-17: preparing the public mirror removed every concrete
identity value (developer account, Team ID, leaf id, certificate
fingerprints, serials) from the tracked tree; they now live only in the
maintainer-local runbook and `~/.config/opencodeserver/signing-identity`.
The Phase 1 detail "Release.xcconfig carries the identity" is superseded:
the xcconfig keeps the ad hoc default and `scripts/build.sh` injects the
configured identity, preserving the measured signing behavior.

## Context

Every `CFBundleVersion` upgrade of the installed product shows one or
more "OpenCodeServerAgent quit unexpectedly" dialogs. Measured on this
machine: launchd SIGKILLs the new agent ~0.9 s after spawn, during
dyld init, with `CODESIGNING / Launch Constraint Violation`, last exit
code 78 `EX_CONFIG`; the existing bounded registration transaction
self-heals in ~15 s.

The external `launchagent-probe` experiment (16 rounds, one fresh
single-use VM clone per round, macOS 26.6.1) established the mechanism:

- With **no Team ID** in the signing identity (ad hoc or self-signed,
  including our stable `OpenCodeServer Local Signing` certificate),
  the SMAppService/BTM record caches a Lightweight Code Requirement
  (LWCR) that pins the helper's **cdHash**. Every build changes the
  cdHash, so every upgrade violates the stale constraint. No
  launchctl/SMAppService operation sequence refreshes it at operation
  time: all four cycle orders (bootout/unregister/register
  permutations) killed the new helper 12/12; `launchctl print`
  showed `has LWCR | needs LWCR update`, i.e. the refresh is
  asynchronous and time-driven. Sequence tricks cannot fix this
  (overturns the earlier "bootout → register" recipe hypothesis).
- With **an Apple-issued identity** (free `Apple Development`
  certificate, Team ID `<team-id>`, not notarized, no hardened
  runtime — deliberately the weakest Apple-issued shape), the same
  worst-case upgrade cycle (no bootout) was kill-free 3/3, converging
  in 2–4 s, while a same-session ad hoc control still crashed.

Conclusion: the launch constraint is **team-anchored when a Team ID
exists** and cdHash-anchored otherwise. The upgrade cascade is a
property of team-less signing, not of our registration transaction.
The product fix is therefore a signing-identity change, not more
lifecycle machinery. traycer's field code corroborates this: its
Developer-ID-signed releases are immune while its ad hoc internal
installs need an elaborate wedge-detection/repair machine.

Current identities:

| | `OpenCodeServer Local Signing` (outgoing) | `Apple Development: <developer-account> (<leaf-id>)` (incoming) |
|---|---|---|
| kind | self-signed RSA 2048 | Apple-issued (WWDR chain), personal team |
| Team ID | none | `<team-id>` (OU) |
| SHA-1 | `<self-signed-sha1>` | `<apple-development-sha1>` |
| serial | `<self-signed-serial>` | `<apple-development-serial>` |
| valid | 2026-07-29 → 2036-07-26 | 2025-08-28 → **2026-08-28 14:41:18 GMT** |

## Decision

1. **Migrate the whole product** — outer app, embedded
   `OpenCodeServerAgent.app`, and `opencodeserverctl` — from
   `OpenCodeServer Local Signing` to the Apple Development identity.
   Whole-bundle, not agent-only: that is the shape the probe
   validated, and it keeps the TCC responsibility chain on a single
   identity. Bundle identifiers, entitlements (empty), architecture,
   and the hardened-runtime flags are unchanged; hardening is
   orthogonal to LWCR anchoring.
2. **Release builds sign with the Apple Development identity,
   `--timestamp=none`** (personal-team identities cannot use secure
   timestamps; the probe arm used the same shape). Debug/test builds
   and CI remain ad hoc (`-`), as AGENTS.md already permits for tests
   that do not exercise TCC.
3. **Expiry-experiment rider (deliberate).** We migrate **now with the
   current, near-expiry certificate** instead of reissuing a fresh one
   first. The ~12 remaining days validate normal operation; after
   2026-08-28 we measure on this machine what an expired identity
   means for the installed product (see Implementation phase 5)
   before reissuing. Rationale: the product will depend on
   annually-expiring free certificates, so expiry behavior must be a
   measured fact, not an assumption. Renewal afterwards is a normal
   Xcode reissue from the same personal team; the Team ID (OU) is
   expected to stay `<team-id>`, so team-anchored DRs and LWCRs
   should survive renewal — a hypothesis phase 6 measures.
4. The old self-signed identity stays in the login keychain for
   rollback/reference but is no longer a product identity. Designated
   Requirements are never weakened to "bundle identifier only"
   (unchanged rule).

## Consequences

- **One-way migration.** The old (self-signed root) and new
  (`anchor apple generic` + OU) designated requirements do not satisfy
  each other. Expected per-machine effects, each covered by existing
  flows and re-tested by phase 4:
  - exactly **one final cascade event** — the stale cdHash-pinned LWCR
    from the last self-signed build kills the first Apple-signed
    spawn, and the existing bounded transaction heals it (~15 s);
    after that, upgrades are expected crash-dialog-free;
  - **TCC**: Local Network / FDA grants attributed to the old identity
    may need one re-grant; the AttributionChain acceptance and the
    ADR 0018 Local Network attribution fallback are re-run on clean
    state (with a Team ID present, attribution may improve);
  - **Keychain**: ADR 0016's grant marker treats the unproven build as
    `access_pending` until one "Allow Keychain Access…" consent; the
    signing-model change triggers ADR 0016's revalidation clause (its
    2026-08-09 partition experiment was measured under
    self-signed/no-Team-ID).
- **Installer gates change with this ADR** (they encoded the old
  "one stable self-signed identity" model): `install.sh` matches the
  first (leaf) `Authority=` line, and `check_requirements.sh` gains
  the narrow identity-change mode above. Both remain fail-closed
  against undeclared identity drift; the mutual-satisfaction rule is
  unchanged for every same-identity upgrade.
- Yearly certificate reissue becomes routine operations; the expiry
  experiment (phase 5) determines how urgent the deadline is. If
  post-expiry spawn validation fails, expiry means product breakage
  until reissue + reinstall, and reissue must happen **before**
  expiry from then on.
- Distribution is unchanged: brew tap installs do not set the
  quarantine attribute, and notarization remains out of scope.
  Adopting Developer ID + notarization later would be another
  identity change handled under this ADR's framework.
- CI and unit/process tests are unaffected (ad hoc, no TCC).

## Implementation plan

Phase 0 — preflight (host):
- `security find-identity -v -p codesigning` lists the identity with a
  private key; the first Release build raises the one-time Keychain
  ACL prompt (approve "Always Allow").
- Record the identity facts table above into the signing-identity runbook
  (maintainer-local; see the 2026-08-17 amendment).

Phase 1 — build wiring and installer gates (keep sources consistent,
env override intact for tests):
- `Config/Release.xcconfig`: set `CODE_SIGN_IDENTITY` to the Apple
  Development identity string; `Base.xcconfig` keeps the ad hoc
  default so Debug/tests/CI are untouched.
- `scripts/build.sh`: default `SIGNING_IDENTITY` to the new identity
  for Release invocations (or stop overriding so the xcconfig wins);
  `scripts/xcode-build-rust.sh` needs no change (inherits
  `EXPANDED_CODE_SIGN_IDENTITY`; timestamp already defaults to none).
- `scripts/install.sh`: `required_signing_authority` → the new leaf
  `Authority=` string, and the extraction must take only the first
  `Authority=` line — Apple-chained identities print the whole
  chain (leaf, WWDR, Apple Root CA), and the previous single-string
  comparison was written for the one-line authority of a self-signed
  certificate.
- `scripts/check_requirements.sh`: mutual designated-requirement
  satisfaction stays the default hard rule for same-identity
  upgrades (and its existing tests). Add an explicit identity-change
  mode (a flag plus the two expected leaf authority strings), used
  only for a signing-identity change governed by an accepted ADR.
  In that mode the script instead requires: both bundles pass
  `codesign --verify --deep --strict`; the previous bundle's leaf
  authority is exactly the outgoing identity and the candidate's is
  exactly the ADR-specified incoming identity; the candidate
  satisfies its own designated requirement; and it prints both DRs
  as the one-way-transition evidence. Any identity drift that does
  not match the declared pair still fails closed.

Phase 2 — candidate build + gates (Build 74):
- Full test suite; `codesign --verify --deep --strict`; dump the
  old-vs-new designated requirements and run `check_requirements.sh`
  in identity-change mode with the ADR 0021 authority pair — the
  mutual non-satisfaction is the expected, recorded evidence.
  Every later same-identity upgrade must return to mutual
  satisfaction under the default mode.

Phase 3 — migration install (this Mac, standard local deployment
workflow; never stop OpenCodeServerAgent or OpenCode):
- Graceful GUI quit → `scripts/install.sh` → reopen → registration
  transaction → verify agent IPC, health, endpoint, bundle versions.
- Measure the expected final crash dialog (DiagnosticReports +
  Unified Logging) and the self-heal time.

Phase 4 — post-migration acceptance:
- Build 75 upgrade: **zero crash dialogs** (the payoff measurement,
  mirroring the probe's certificate arm).
- Clean-state TCC AttributionChain and Local Network attribution
  re-test (revisit ADR 0018), FDA probe tri-state, Keychain
  `access_pending` consent flow, DR compatibility between builds 74
  and 75.

Phase 5 — expiry experiment (certificate expires 2026-08-28
14:41:18 GMT):
- Before expiry: an expiring-cert build stays installed and
  registered; snapshot TCC rows, Keychain grant marker, and DRs.
- After expiry, measure and record (the signing-identity runbook + a test
  record): (a) does the running agent keep running? (b) logout/login
  respawn? (c) reboot respawn? (d) is the OpenCode child unaffected?
  (e) `codesign --verify` on the installed bundle; (f) does
  Xcode/codesign refuse to sign with the expired identity (expected:
  yes)?
  Hypotheses to test, not assume: kernel exec is cdHash-structural and
  unaffected; chain-validating checks (DR/LWCR at current time, TCC)
  may fail, possibly re-enabling the cascade on spawn.

Phase 6 — renewal:
- Reissue from Xcode with the same personal team; record new identity
  facts; build and ship; measure reauthorization churn (hypothesis:
  LWCR/TCC survive the same-OU renewal; Keychain requires at most one
  consent per ADR 0016).

Documentation updated during implementation: the AGENTS.md "Code
signing identity" section and the quality-gates rule ("build N and
build N+1 satisfy each other's expected requirements" gains the
carve-out that a signing-identity change governed by an accepted ADR
instead verifies the exact expected old/new leaf identities and
records the transition), the signing-identity runbook (identity facts,
Xcode reissue procedure, Apple-rooted trust scope, yearly expiry
cadence; .p12 backup becomes low priority since reissue is free),
`PRODUCT_DECISIONS.md`, `CHANGELOG.md`.

## Post-implementation measurement — Local Network acceptance (2026-08-16)

Phase 4's clean-state Local Network attribution acceptance ran as a
dual-group experiment (`~/Projects/localnetwork-probe/`, results
committed there): E = this ADR's Apple Development identity, C = the
outgoing self-signed identity, one source commit (`f3dae0d`), two
separately frozen clean baselines, four interleaved rounds. Result:

- **FAIL for exact product-name attribution** in the one E round whose
  title was observable (E2: `允许"OpenCodeServerAgent" 查找本地网络中的
  设备?`); E1's title was not retained by the operator and is excluded.
- The control reproduced the **same** alert title and Settings row —
  signing identity did not discriminate. The acceptance line is
  therefore classified under AGENTS.md's measured-platform-limitation
  framework (agent-name attribution in a
  LaunchAgent-launches-external-child architecture, unchanged by the
  Team ID), not as a regression caused by this migration. The ADR 0018
  amendment of the same date records the platform-behavior update, and
  AGENTS.md's Local Network paragraph was revised to match.
- What this ADR set out to fix is unaffected and measured: the upgrade
  kill cascade is gone (Build 75 zero-dialog upgrade; the only cascade
  event after migration was the one predicted final kill at Build 74's
  install), FDA chain evidence resolves
  `responsible = ai.opencode.server` under the new identity
  (`fda = verified`), and the Keychain one-time consent behaved per
  ADR 0016.

## Post-implementation measurement — clean-VM upgrade differential (2026-08-16)

A second post-implementation experiment
(`~/Projects/upgrade-crash-probe/`) re-verified the core claim on
clean virtual-machine state with the real product: four interleaved
rounds (E1/C1/E2/C2), each on a fresh single-use clone of the frozen
`LN-E-baseline`/`LN-C-baseline` VMs, Release builds 76 → 77 from
`main` `55f472b` (version bumps on a throwaway branch, never merged),
identical procedures for both arms — GUI quit, atomic whole-bundle
swap, reopen, GUI version-change transaction — differing only in
signing identity.

- **E (Apple Development): PASS 2/2.** Zero new OpenCodeServerAgent
  crash reports, no operator-observed dialog, authenticated healthy
  status at bundle 77, convergence in 2.6 s and 1.026 s.
- **C (self-signed control): failure reproduced 2/2.** One new crash
  report and the verbatim dialog `“OpenCodeServerAgent”意外退出。` each
  round; Unified Logging recorded the AMFI rejection
  (`The file is adhoc signed or signed by an unknown certificate
  chain`, and `no eligible provisioning profiles found`); recovery
  only via the GUI transaction's second bounded retry at ~34 s.
- Measurement deviation, recorded per the probe's RESULTS: the literal
  `needs LWCR update` launchd string was **not** present in these runs
  (`has LWCR` was). The verdict does not rest on any platform string —
  only on the same-source, same-procedure differential.

This is the third independent evidence layer for the ADR's core
mechanism claim: the synthetic probe (16 rounds), the real-machine
Build 74/75 migration, and now a clean-VM real-product differential.

## References

- launchagent-probe experiment, 16 rounds + amendments (local project
  `~/Projects/launchagent-probe/`, RESULTS.md, Q1–Q6; Q6 = the
  certificate arm).
- ADR 0016 (Keychain credential storage; revalidation clause).
- ADR 0018 (Local Network privacy attribution; fallback measured
  under self-signed/no-Team-ID).
- the signing-identity runbook (maintainer-local copy outside the
  repository; in-repo template `docs/signing-identity.example.md`),
  now covering the active Apple Development identity.
- TN3127 (inside code signing requirements); WWDC23 launch
  constraints session.
