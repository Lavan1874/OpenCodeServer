# Local Network attribution — Agent-only diagnostic probe (Build 66)

Date: 2026-08-12  
Target: macOS 26.6.1 (25G76), Apple Silicon  
Purpose: distinguish an external-OpenCode-child attribution problem from a
self-signed/no-Team-ID Agent identity problem.

## Test isolation

- Preserved baseline: `macOS 3.utm` (`macOSvm3`)
- Deleted before the test: the previous experiment, control, and single-version
  experiment VMs
- New VM: `OpenCodeServer Agent Local Network 诊断.utm`
- New VM UUID: `8D4D1CC2-3F42-42D3-BF97-2CF02A0E1C8A`
- New VM MAC: `5a:bb:2c:f6:54:01`
- VM address during the run: `192.168.64.8`
- Only this VM was running during the test

The new VM was cloned from the macOSvm3 baseline. The baseline itself was not
modified.

## Diagnostic build

Build 66 was compiled with the Cargo feature
`diagnostic-local-network`. Normal product builds do not enable this feature.
Before any OpenCode child was launched, OpenCodeServerAgent started a bounded
worker that:

1. bound a UDP socket;
2. joined `224.0.0.251`;
3. sent four bytes to UDP port 5353;
4. waited 300 ms; and
5. left the multicast group and exited the worker.

Build evidence:

```text
CFBundleVersion = 66
Agent LC_UUID = F3BBF6B5-A96B-3076-B232-DEAD930A2716 (arm64)
Signing authority = OpenCodeServer Local Signing
TeamIdentifier = not set
codesign --verify --deep --strict = passed
Bundle validation = passed
Rust tests with diagnostic feature = 69 passed
```

## Procedure

1. Installed the signed diagnostic Build 66 into `/Applications/OpenCodeServer.app`.
2. Launched OpenCodeServer. OpenCode was not launched before the Agent probe.
3. Re-registered the changed Agent through the normal Service Management
   update transaction. The first stale launch-constraint attempt failed; the
   bounded retry registered Build 66 and authenticated IPC became reachable.
4. Observed the Local Network system authorization prompt and collected the
   Agent Unified Log.

## Evidence

The prompt displayed **`OpenCodeServerA`**, not `OpenCodeServer`. The Agent was
the only OpenCodeServer product component performing the multicast operation.

Relevant VM Unified Log entries:

```text
OpenCodeServerAgent[1227] [ai.opencode.server:agent] OpenCodeServerAgent started
OpenCodeServerAgent[1227] [ai.opencode.server:agent] Diagnostic Local Network probe failed: No route to host (os error 65)
nehelper: No team ID found for (bundleID: ai.opencode.server.agent, name: OpenCodeServerA)
nehelper: Found path /Applications/OpenCodeServer.app/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent for PID 1227, will prompt
nehelper: Local network preference not yet set, prompting for OpenCodeServerA (ai.opencode.server.agent)
```

The first implementation only joined the multicast group and completed without
a prompt. It was discarded as insufficient evidence because it emitted no
network traffic. The Build 66 implementation sent actual multicast traffic;
the send returned `ENETUNREACH` after `nehelper` had entered the prompt path.

## Result

The Agent-only operation still produced the truncated Agent identity. This
falsifies the narrow hypothesis that the Local Network attribution failure is
caused only by the external OpenCode grandchild. In this tested
self-signed/no-Team-ID configuration, the Agent's own network operation is
also not presented as `OpenCodeServer`.

The result supports the signing-strength/identity-chain hypothesis. It does
not prove that Developer ID alone will fix every external-child case, and it is
not a universal claim about other signing models, keychains, or macOS builds.

The detailed architectural consequence is recorded in
`docs/adr/0018-local-network-privacy-attribution.md` and
`PRODUCT_DECISIONS.md`: the SMAppService parent-bundle record and Local Network
responsible-code attribution are separate mechanisms and must be verified
independently.
