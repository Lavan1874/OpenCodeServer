# OpenCodeServer architecture notes

The behavior-level detail behind the README: how supervision, Service
Management registration, IPC, and runtime state actually work, and the
reasoning behind each decision. Each topic links to its ADR for the full
decision record.

## Components and responsibility boundary

OpenCodeServer is one standard macOS `.app` with two long-running native
components and one auxiliary client:

- `OpenCodeServer` — the Swift/AppKit menu bar GUI, settings editor,
  Service Management registration coordinator, and IPC client.
- `OpenCodeServerAgent` — the launchd-managed Rust OpenCode manager,
  supervisor, runtime-state authority, Full Disk Access probe owner, and
  IPC server.
- `opencodeserverctl` — the short-lived command-line IPC client. It never
  manages OpenCode processes or runtime-state files directly.

The boundary is strict: OpenCodeServerAgent is the only OpenCode process
manager and the only authority for OpenCode runtime state. OpenCodeServer
presents status, edits configuration, sends IPC commands, and manages
`SMAppService` registration only for first registration, a real
bundle-version change, a verified registration error, or an explicit
"Repair OpenCodeServerAgent" action. It never inspects or signals OpenCode
PIDs and never treats IPC unavailability as registration corruption.
Quitting OpenCodeServer unexpectedly never stops OpenCode. OpenCode itself
is independent and has no knowledge of any of these components.

Both long-running components register independently at login; no startup
ordering is assumed (ADR 0008).

## Process supervision

OpenCodeServerAgent (ADR 0003, ADR 0009):

- executes the configured absolute native Mach-O path directly — never
  through a shell or script interpreter;
- runs OpenCode in a dedicated process group and forwards termination to
  the whole tree, as cooperative lifecycle management for a trusted
  installation;
- allows a bounded graceful-shutdown interval, and never sends `SIGKILL`
  merely because the deadline expired — force termination requires a
  second explicit user action or `opencodeserverctl --force`;
- classifies normal exit, signaled exit, explicit stop, and startup
  failure, and reaps the direct child correctly;
- recovers a crashed OpenCode with bounded `1, 2, 5, 15, 30` second
  attempts.

Before crash reattachment, the recorded process identity is classified
first: PID, process start time, executable, process group, and user (ADR
0005, ADR 0014). Missing or identity-mismatched records are cleared
without signaling. A live match must then pass the versioned canonical
configuration fingerprint, authenticated health, and a second identity
check. An identity-VERIFIED but configuration-mismatched process is not
abandoned: it is taken over as a managed stale-configuration process
(reported as `config_pending`, stoppable and restartable) and rechecked
once credentials converge.

Supervision is designed for a trusted OpenCode installation, not hostile
native-code containment (ADR 0015). Descendants that remain in the
authorized process group are managed as a unit; an observed group escape
or identity anomaly fails closed and never authorizes a signal to a
foreign group.

## Service Management registration and updates

Registration uses `SMAppService.agent` with the LaunchAgent plist embedded
in the app bundle (ADR 0001, ADR 0006). Ordinary status monitoring never
mutates registration. A real bundle-version update is one persistent,
bounded transaction, because macOS 26 can accept a changed embedded
LaunchAgent registration while retaining stale launch metadata:

- at most three state-observed unregister/register attempts per
  transaction, each waiting for asynchronous unregistration, observing
  `notRegistered`, allowing bounded settling, and requiring authenticated
  OpenCodeServerAgent IPC;
- the attempt number is persisted, so restarting OpenCodeServer cannot
  reset the budget;
- the pending version becomes `RegisteredBundleVersion` only after an
  authenticated IPC peer proves it is the pending build.

An enabled, same-version OpenCodeServerAgent that is temporarily
unreachable appears as starting or temporarily unavailable while
OpenCodeServer keeps monitoring IPC — never as registration corruption.

## IPC

OpenCodeServer, OpenCodeServerAgent, and opencodeserverctl speak one
versioned protocol over a user-owned Unix domain socket in Application
Support (mode `0600`), authenticated with a same-user `getpeereid` check.
Messages are bounded (64 KiB) and status responses never contain secrets.
Status is pushed to subscribers with bounded reconnect (ADR 0010), so the
menu reflects agent-authoritative state rather than polling guesswork.

## Credentials and on-disk state

- The password lives only in the login Keychain as a Generic Password
  item (ADR 0016). Updates use `SecItemUpdate` in place, and a real change
  is propagated to the agent through a non-interactive `credential_changed`
  notice; an unchanged save is a no-op.
- Background decrypt-class reads never run on the supervisor event loop
  and never raise a consent dialog unproven: routine work uses an
  attribute-only probe, and a decrypt-class read runs silently only when
  the persisted grant marker proves a prior decrypt for this account and
  build, or when the marker's recorded Team ID matches the running
  build's signing team (one automatic read per account per process run).
  Every unproven case falls back to the explicit Settings "Allow Keychain
  Access…" action. A missing grant is the soft `access_pending` state.
- `config.plist`, the private fingerprint key, and runtime state are mode
  `0600` under mode-`0700` directories. Runtime state carries a versioned
  HMAC-SHA256 tag over canonical launch semantics; neither the password
  nor the HMAC key is placed there. Runtime state that does not match the
  current required schema is rejected — there is no metadata fallback or
  state migration.

## Privacy and verification posture

- No App Sandbox by design: the product launches developer tooling for
  arbitrary user-selected projects. No firewall configuration, no TCC
  database reads or writes, no System Settings UI automation, no project
  directory scanning.
- Full Disk Access is displayed as a tri-state (`Verified` /
  `Not Verified` / `Unable to Determine`), verified only by
  OpenCodeServerAgent through a minimal read-only functional probe (ADR
  0002). A failed probe is not proof that the FDA switch is off.
- The menu never reveals the real password length, and the password row
  appears only while authorization is pending.
- Password and credentials never appear in process listings, logs,
  snapshots, IPC status, or the signed app.

## Release engineering

Xcode owns Swift/AppKit compilation, the `.app` bundle, generated
`Info.plist`, `LSUIElement`, entitlements, tests, and the outer signature.
A Cargo Run Script phase builds, copies, and signs OpenCodeServerAgent and
opencodeserverctl before the outer signing, and any failure fails the
build. Candidates are gated by strict bundle validation, `codesign
--verify --deep --strict`, and mutual Designated Requirement checks; the
concrete signing identity is intentionally not part of the tracked tree
(`docs/signing-identity.example.md`, ADR 0021). Privacy attribution,
TCC, and Service Management acceptance require the documented clean-machine
manual gates in [ACCEPTANCE.md](ACCEPTANCE.md).
