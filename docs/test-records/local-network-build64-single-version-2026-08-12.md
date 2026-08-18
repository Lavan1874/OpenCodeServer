# Local Network attribution retest — Build 64, single-version VM

Date: 2026-08-12  
Target: macOS 26.6.1 on Apple Silicon  
Purpose: determine whether the earlier Build 62/Build 64 coexistence was
necessary to reproduce the `OpenCodeServerAgent` row in Local Network privacy
settings.

## Setup

- Source baseline: `macOSvm3` (`macOS 3.utm`), duplicated without modifying the
  baseline VM.
- Test VM: `OpenCodeServer 单一版本实验组.utm`
- Test VM UUID: `C3B3BFFC-2C98-4380-810E-8E618078B7BB`
- Test VM MAC: `5a:bb:2c:f6:53:01`
- Test VM address during the run: `192.168.64.7`
- Before installation, the clone had no OpenCodeServer app, persisted
  OpenCodeServer configuration, or registered OpenCodeServerAgent.
- Installed products: only the signed Build 64 `OpenCodeServer.app` and
  OpenCode 1.18.16. No Build 62 or other OpenCodeServer version was present.

## Procedure and observations

1. Started OpenCodeServer and confirmed OpenCodeServerAgent/OpenCode were
   healthy on the loopback default.
2. In Settings, enabled mDNS and changed the listener to `192.168.64.7`.
3. Chose the explicit restart action. The macOS Local Network alert displayed
   **OpenCodeServerA**. The screenshot shows the historical truncated title,
   generic network icon, and generic macOS Local Network copy; it did not use
   the intended `OpenCodeServer` responsible-app identity.
4. Chose Allow, then opened System Settings → Privacy & Security → Local
   Network. The list contained one `OpenCodeServerAgent` row with its toggle
   off; no `OpenCodeServer` row was present.
5. Restored the listener to `127.0.0.1`, disabled mDNS, restarted OpenCode,
   confirmed healthy status, and stopped the VM.

The screenshots captured during the run are retained locally at:

- `/tmp/OpenCodeServer-Build64-single-LocalNetwork-20260812.png`
- `/tmp/OpenCodeServer-Build64-single-SystemSettings-20260812.png`

## Result

The Build 64 single-version run reproduced both observed attribution problems:
the authorization alert showed `OpenCodeServerA`, and System Settings showed an
`OpenCodeServerAgent` row. This is a failing observation for the intended
responsible-app identity. It also does not support treating Build 62/Build 64
coexistence mentioned by TN3179 as the sole explanation, although the baseline
was not an erased TCC state. The remaining macOS responsibility-chain cause is
not established and the acceptance gate remains open.

The VM was cloned from the macOSvm3 baseline, not from a newly erased TCC
database. The pre-existing Local Network row was therefore not independently
proven absent before installation. The result is consequently scoped to the
tested restorable baseline and must not be generalized to every Local Network
privacy-history state.

## Structural prerequisite checks

The installed Build 64 LaunchAgent was inspected with
`launchctl print gui/501/ai.opencode.server.agent`. Its relevant fields were:

```text
managed_by = com.apple.xpc.ServiceManagement
parent bundle identifier = ai.opencode.server
parent bundle version = 64
```

This confirms that SMAppService associated the Agent with the main app. The
record did not expose a separate delegate-app field.

`dwarfdump --uuid` confirmed arm64 `LC_UUID` values and no duplicate UUID in the
tested product-binary set. The installed Build 64 Agent was
`94A1D912-4D76-3FB6-A0E1-2F5F33F85A12`; the signed Build 65 Release candidate
Agent was `7FDE535D-5581-3571-AAF0-C773B98E156A`; external OpenCode 1.18.16
was `C7E7A979-F99B-3466-9AD6-E56A63373A35`. Two clean Rust Release builds
with the same `OPENCODESERVER_BUNDLE_VERSION=65` input both produced
`5D1914AA-A658-3BFA-AAAC-3F2A4CDED0E7`, while Build 64 and Build 65 produced
different Agent UUIDs.

The structural checks therefore pass. They do not change the observed
self-signed/no-Team-ID UI fallback, which is recorded as a platform/signing
limitation for the current external-child responsibility chain.
