# ADR 0018: Local Network authorization attribution

Date: 2026-08-12

## Status

The bundle structure is compliant with the documented SMAppService layout, and
the SMAppService/UUID prerequisites are verified. Exact upward UI attribution
is not reachable in the current self-signed/no-Team-ID architecture where
OpenCodeServerAgent launches an external OpenCode grandchild process; it remains
a platform/signing limitation rather than an active bundle-structure repair
target.

## Context

When mDNS is enabled, the managed OpenCode process performs a Bonjour/local
network operation. macOS does not choose the text in the Local Network alert
from an arbitrary string supplied by the application. It determines the
responsible code for the operation, then uses that identity, its icon, and its
`NSLocalNetworkUsageDescription` when presenting the alert and recording the
permission.

Apple's [TN3179: Understanding local network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
describes the intended relationship: when an app launches a helper that
performs the network operation, the app can be the responsible code. For a
`launchd` agent, Apple's guidance also calls out
`AssociatedBundleIdentifiers`. Apple further recommends an Apple-issued
code-signing identity when reliable identity tracking is required; the current
project's self-signed/no-Team-ID test is therefore an acceptance environment,
not proof that every signing model behaves identically.

Build 62 exposed the failure mode. The launch agent ran a bare Mach-O at
`Contents/MacOS/OpenCodeServerAgent`. On macOS 26, the Local Network alert
fell back to the executable identity and displayed the truncated name
`OpenCodeServerA`, a generic network icon, and generic copy. The generated
main-app `Info.plist` also did not contain
`NSLocalNetworkUsageDescription`. Merely renaming the executable or adding a
purpose string to the main app could not establish the responsible-code
relationship.

## Decision

Represent the background component as a real nested app-like bundle and make
the parent app relationship explicit at every supported bundle boundary.

The release layout is:

```text
OpenCodeServer.app/
  Contents/
    MacOS/OpenCodeServer
    MacOS/opencodeserverctl
    Resources/OpenCodeServerAgent.app/
      Contents/
        Info.plist
        PkgInfo
        MacOS/OpenCodeServerAgent
    Library/LaunchAgents/ai.opencode.server.agent.plist
```

The nested helper bundle has:

- `CFBundleDisplayName = OpenCodeServer`
- `CFBundleName = OpenCodeServer`
- `CFBundleIdentifier = ai.opencode.server.agent`
- `CFBundleExecutable = OpenCodeServerAgent`
- `CFBundlePackageType = APPL`
- the same concise `NSLocalNetworkUsageDescription` as the main app

The embedded launch-agent property list now uses:

```text
BundleProgram = Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent
AssociatedBundleIdentifiers = [ai.opencode.server]
```

The main app's generated `Info.plist` also carries:

```text
NSLocalNetworkUsageDescription =
  OpenCodeServer advertises your configured OpenCode service on the local network when you enable mDNS.
```

The Xcode Rust build phase creates and signs the nested helper bundle before
the outer app is signed. It does not leave a second bare
`Contents/MacOS/OpenCodeServerAgent` copy. Bundle validation, installation
validation, and the Keychain ACL trusted-application path all use the nested
helper path. The old bare-executable `__TEXT,__info_plist` workaround was
removed; the bundle structure, launch-agent association, and signed metadata
are now the source of identity.

The runtime interaction remains deliberately progressive:

1. mDNS is off by default.
2. The user expands `Advanced`, enables mDNS, and saves the configuration.
3. OpenCodeServer asks the user to restart OpenCode when required; Save itself
   does not silently request Local Network permission.
4. After the explicit restart, OpenCodeServerAgent launches OpenCode with the
   configured mDNS option.
5. Only then may macOS present the Local Network authorization prompt.

The application never writes the TCC database, changes the permission
programmatically, or guesses an `NSBonjourServices` value. The prompt must be
caused by the real configured mDNS operation.

## Evidence

### Agent-only diagnostic probe (Build 66, 2026-08-12)

To separate the external-child hypothesis from the signing/identity
hypothesis, a diagnostic-only Build 66 was compiled with the
`diagnostic-local-network` Cargo feature. On Agent startup, before any
OpenCode child was launched, `OpenCodeServerAgent` bound a UDP socket, joined
`224.0.0.251`, sent a four-byte probe to UDP port 5353, waited 300 ms, and
exited the probe worker. The feature is excluded from normal builds.

The probe ran in a new clone of the `macOSvm3` baseline:

- VM: `OpenCodeServer Agent Local Network 诊断`
- macOS: 26.6.1 (25G76), Apple Silicon
- Build: 66, self-signed, no Team ID
- Agent: `ai.opencode.server.agent`, `OpenCodeServerAgent.app`
- Agent `LC_UUID`: `F3BBF6B5-A96B-3076-B232-DEAD930A2716`

The resulting system alert still displayed **`OpenCodeServerA`**, even though
the Agent itself was the only product process performing the multicast
operation. Unified Log evidence from the VM records:

```text
nehelper: No team ID found for (bundleID: ai.opencode.server.agent, name: OpenCodeServerA)
nehelper: Found path /Applications/OpenCodeServer.app/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent for PID 1227, will prompt
nehelper: Local network preference not yet set, prompting for OpenCodeServerA (ai.opencode.server.agent)
OpenCodeServerAgent: Diagnostic Local Network probe failed: No route to host (os error 65)
```

The first probe attempt only joined the group and completed without a prompt;
that was insufficient evidence because it did not emit traffic. The Build 66
probe emitted an actual multicast send and produced the attribution prompt.
The send returned `ENETUNREACH` after the system had already entered the
Local Network prompt path; this does not change the responsible-code evidence.

This diagnostic result falsifies the narrow claim that the problem is caused
only by an external OpenCode grandchild. In the tested self-signed/no-Team-ID
environment, even an Agent-originated operation is presented as the truncated
Agent identity. It supports the signing-strength/identity-chain hypothesis;
an Apple-issued signing model or another supported responsibility-chain change
is now a prerequisite for testing whether upward attribution can work. It does
not prove that Developer ID alone will fix every external-child case, and it
does not establish a universal rule for other signing or macOS configurations.

The earlier two-VM comparison on macOS 26.6.1 was recorded as follows. The
Build 64 prompt entry in that record was later found to be a screenshot
misreading and is corrected by the single-version evidence below:

| VM | Build | Observed Local Network prompt |
| --- | ---: | --- |
| `OpenCodeServer 对照组` | 62 | `OpenCodeServerA`, generic icon/copy |
| `OpenCodeServer 实验组` | 64 | Earlier note said `OpenCodeServer`; that attribution was misread |

The follow-up single-version run duplicated the `macOSvm3` baseline into
`OpenCodeServer 单一版本实验组`. Before installation it had no
OpenCodeServer app, configuration, or registered OpenCodeServerAgent. Only
Build 64 and OpenCode 1.18.16 were installed. The alert again named
`OpenCodeServerA`, and System Settings → Privacy & Security → Local Network
still showed one `OpenCodeServerAgent` row. This reproduces both attribution
problems in a single-version app/config run. Because the macOSvm3 baseline
was not an erased TCC database, this does not by itself prove that the row was
created by this installation or that coexistence is unnecessary in every
privacy-history state.

### Structural prerequisite checks (2026-08-12)

The installed Build 64 registration was inspected with:

```text
launchctl print gui/501/ai.opencode.server.agent
```

The record reported:

```text
managed_by = com.apple.xpc.ServiceManagement
parent bundle identifier = ai.opencode.server
parent bundle version = 64
```

Therefore SMAppService did associate the LaunchAgent with the main app;
`AssociatedBundleIdentifiers` was not ignored. The record does not expose a
separate `delegate app` field, but its parent-bundle field is the relevant
runtime association.

This association is deliberately not treated as a Local Network attribution
result. The `SMAppService` parent-bundle record answers which main app owns and
registers the launchd service. Local Network responsible-code attribution is a
separate securityd/TCC decision about the code that performs the network
operation (here, the external OpenCode child). A correct
`parent bundle identifier = ai.opencode.server` therefore does not imply that
the system authorization alert or the Local Network settings row will be named
`OpenCodeServer`. These are separate acceptance surfaces and must be verified
independently; adjusting the launchd parent relationship or nested bundle
layout alone is not evidence that the network attribution chain has changed.

`dwarfdump --uuid` found arm64 `LC_UUID` values on all tested native binaries:

| Binary | Build/version | UUID |
| --- | --- | --- |
| installed OpenCodeServerAgent | Build 64 | `94A1D912-4D76-3FB6-A0E1-2F5F33F85A12` |
| Release candidate OpenCodeServerAgent | Build 65 | `7FDE535D-5581-3571-AAF0-C773B98E156A` |
| installed OpenCodeServer | Build 64 | `B5A304EC-4BCB-3FC1-AC11-D44EA580B0D0` |
| Release candidate OpenCodeServer | Build 65 | `25622A5F-0737-3653-AFC4-04B2ACA41B78` |
| installed OpenCode 1.18.16 | external child | `C7E7A979-F99B-3466-9AD6-E56A63373A35` |

There were no duplicate UUIDs in the tested product-binary set. Two clean
Release Rust builds with the same `OPENCODESERVER_BUNDLE_VERSION=65` input
both produced Agent UUID `5D1914AA-A658-3BFA-AAAC-3F2A4CDED0E7`, while the
Build 64 and Build 65 linked Agents had different UUIDs. This is the expected
stable-and-content-sensitive behavior; missing or duplicate UUID is not the
cause of the observed attribution. This matches TN3179's build-time guidance:
reliable macOS identity tracking requires an Apple-issued signing identity, and
the main executable must have a unique UUID.

In the Build 64 run, allowing the prompt left OpenCodeServerAgent and OpenCode
healthy. Returning to `127.0.0.1`, disabling mDNS, and restarting did not raise
another prompt. These observations are recorded in
`docs/ACCEPTANCE.md` under “Local Network privacy and mDNS”.

The authorization-alert surface is not corrected by Build 64 in this test:
the historical `OpenCodeServerA` fallback remains reproducible. The earlier
statement that this alert was corrected was a misreading of the screenshot and
is withdrawn. The bundle structure is retained as Apple-documented, compliant,
and inspectable, but further nested-bundle, executable-renaming, or purpose-string
changes are not treated as a sufficient remedy under the current signing and
responsibility chain.

## Remaining limitation

The single-version app/config run showed `OpenCodeServerA` in the alert and
`OpenCodeServerAgent` as the row name in System Settings → Privacy & Security →
Local Network. This is direct evidence that the intended product identity is
not established on the tested Build 64 path, despite the structural
prerequisites passing. The clone came from the macOSvm3 baseline rather than
an erased TCC database, and the pre-existing row was not independently proven
absent; the observation is scoped to that restorable privacy-history state.
Under the current self-signed/no-Team-ID plus external-child architecture,
this UI result is recorded as a platform limitation. A future Apple-issued
signing model or responsibility-chain change reopens the UI attribution gate;
until then, no additional bundle-structure patch is promised.

## Consequences

- The historical truncated `OpenCodeServerA` prompt remains reproducible on
  the tested Build 64 alert path, so the alert-attribution defect is not closed.
- The helper has a stable, inspectable bundle identity and can be signed and
  validated as nested code before the outer app.
- The nested path is part of the current bundle and Keychain ACL contract; all
  three product binaries are updated together without compatibility branches.
- `NSLocalNetworkUsageDescription` improves explanation but is not, by itself,
  a responsible-code mechanism.
- Self-signed/no-Team-ID behavior must continue to be treated as a measured
  test result. A future Developer ID or Team ID signing change requires fresh
  Local Network attribution and TCC acceptance testing.

## Amendment — 2026-08-16: Team-ID retest (dual-group, clean state)

The signing-model change contemplated above has now happened (ADR 0021,
Apple Development identity, Team ID `<team-id>`), and the required fresh
Local Network attribution test was executed as a dual-group clean-VM
experiment (`~/Projects/localnetwork-probe/`, four interleaved rounds
E1/C1/E2/C2, two separately frozen baselines, one source commit
`f3dae0d`, differing only in signing identity):

- The historical truncated `OpenCodeServerA` alert title did **not**
  reproduce — not even for the self-signed control group.
- The current stable behavior on macOS 26.6.1 (25G76) is identical for
  BOTH identities: the alert is titled
  `允许"OpenCodeServerAgent" 查找本地网络中的设备?` (verbatim, guest in
  Chinese locale) and System Settings shows exactly one enabled row
  named `OpenCodeServerAgent`.
- Signing identity is therefore not the discriminator for Local Network
  UI attribution in this architecture; the agent-name attribution is a
  platform limitation of a LaunchAgent launching an external OpenCode
  child, now measured under both no-Team-ID and Team-ID signing.
- Structural prerequisites held in every round (BTM parent =
  `ai.opencode.server@75`; participating arm64 Mach-Os carried present,
  unique `LC_UUID`s), and tccd `AUTHREQ_ATTRIBUTION` machine evidence
  resolves `responsible = ai.opencode.server` under both identities.
  The `accessing = /opt/homebrew/Cellar/opencode/...` chain line is not
  exposed in collectable logs on this OS version — not-observable, not
  failed.

This amendment supersedes the `OpenCodeServerA`-era title expectation.
The Status framing ("platform/signing limitation") is retained, with
"platform" now the measured operative word; a signing-model change no
longer requires re-running this UI test merely because the Team ID
changed — the re-run trigger is a responsibility-chain change.

## Implementation references

- `resources/ai.opencode.server.agent.plist`
- `resources/OpenCodeServerAgent-Info.plist`
- `scripts/xcode-build-rust.sh`
- `scripts/validate_bundle.sh`
- `scripts/install.sh`
- `swift/Sources/OpenCodeServer/KeychainStore.swift`
- `docs/ACCEPTANCE.md`
