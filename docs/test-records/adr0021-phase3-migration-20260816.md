# ADR 0021 — Phase 3 migration install

Date: 2026-08-16  
Target: macOS 26.6.1 (25G76), Apple Silicon  
Branch: `adr0021/signing-migration`  
Candidate: Build 74

## Deployment

The installed Build 73 `OpenCodeServer` GUI received a normal quit request and
exited before replacement. No stop or signal action was sent to
`OpenCodeServerAgent` or `OpenCode`.

The migration-specific installer invocation was:

```text
./scripts/install.sh --identity-change \
  "OpenCodeServer Local Signing" \
  "Apple Development: <developer-account> (<leaf-id>)" \
  ~/Projects/opencode_server/build/OpenCodeServer.app
```

Bundle validation, nested signature validation, the exact outgoing/incoming
leaf-authority checks, and the one-way Designated Requirement gate passed. The
installer atomically replaced `/Applications/OpenCodeServer.app` and reported
`CFBundleVersion 74`.

## Registration and authenticated IPC

OpenCodeServer was reopened at 2026-08-16 11:27:52 +0800. The first
registration attempt was accepted by `SMAppService` but its authenticated IPC
verification was not yet successful. OpenCodeServer performed the bounded
second registration attempt; the Agent then answered authenticated IPC and the
registration transaction settled.

Evidence after convergence:

```text
launchctl: managed_by = com.apple.xpc.ServiceManagement
launchctl: parent bundle identifier = ai.opencode.server
launchctl: parent bundle version = 74
launchctl: job state = running
OpenCodeServer CFBundleVersion = 74
OpenCodeServerAgent authenticated status.bundle_version = 74
RegisteredBundleVersion = 74
endpoint = 10.0.0.254:4096
installed_version = 1.18.18
running_version = 1.18.18
```

The Agent's nested app does not carry an independent `CFBundleVersion` key;
its build identity is the parent bundle version baked into the
`OpenCodeServerAgent` binary and reported by authenticated protocol-5 status.
This matched the Service Management parent bundle version (`74`).

At the end of this phase the status was deliberately:

```text
server_state = unhealthy
health = unknown
password_state = access_pending
config_pending = true
last_error = OpenCode is running with its previous configuration — grant Keychain access, then restart
```

This is the ADR 0016 post-identity-change state: routine background Keychain
work does not raise a dialog, and the explicit consent flow is measured in
Phase 4. The existing OpenCode process remained managed with its previous
configuration; no attempt was made to repair or restart it during this phase.

## Expected crash measurement

Exactly one new `OpenCodeServerAgent` DiagnosticReports file appeared after
the migration install and reopen:

```text
OpenCodeServerAgent-2026-08-16-112754.ips
timestamp = 2026-08-16 11:27:54 +0800
bundleID = ai.opencode.server.agent
bug_type = 309
```

Unified Logging recorded the expected sequence: registration attempt 1,
authenticated-IPC verification not yet available, bounded attempt 2,
registration attempt 2, Agent credential state `AccessPending`, and
authenticated IPC verification for bundle version 74 at 11:28:09.279 +0800.
The measured self-heal interval from the crash report timestamp to verified
IPC was approximately 15.3 seconds (17.3 seconds from the reopen command).

This was the one expected old-cdHash registration event. No repair action,
manual Agent restart, or OpenCode stop was performed.

## Phase 3 result

The identity migration install, bounded Service Management transaction,
authenticated Agent IPC, bundle-version verification, endpoint verification,
and the expected single crash/self-heal event passed. The health check remains
explicitly pending the Phase 4 Keychain consent flow; no credential or secret
was recorded.
