# OpenCodeServer

> **Unofficial project.** OpenCodeServer is an independent third-party
> utility. It is not affiliated with, endorsed by, or supported by the
> OpenCode project (opencode.ai).

OpenCodeServer is a native macOS 26 menu bar utility that keeps one
Homebrew-installed OpenCode running independently of OpenCodeServer.

The first version contains:

- `OpenCodeServer`, a Swift/AppKit menu bar GUI and native settings
  window;
- `OpenCodeServerAgent`, a synchronous Rust OpenCode manager managed by `launchd`
  through `SMAppService`;
- `opencodeserverctl`, a small native command-line client for SSH and local
  administration.

The app is Apple Silicon-only, intentionally not App Sandbox-enabled, and does
not modify OpenCode configuration, firewall rules, TCC databases, projects,
providers, models, sessions, or plugins.

## Current implementation

The v1 source implementation includes:

- a native Xcode 26 macOS Application project with an AppKit Application
  Target, hosted XCTest Target, shared Scheme, asset catalog, generated
  `Info.plist`, entitlements, and text-reviewable `.xcconfig` settings;
- the Xcode 26 AppKit template lifecycle (`@main` `NSApplicationDelegate`) and
  a Main storyboard containing the standard Application, Edit, and applicable
  Window menus;
- standard Edit commands routed to the current field editor through AppKit’s
  Responder Chain, including the secure and revealed password controls;
- independent login registration for OpenCodeServer and OpenCodeServerAgent;
- a persistent, observable OpenCodeServerAgent update coordinator that runs
  only for initial registration, an actual Bundle version change, or explicit
  repair, and commits `RegisteredBundleVersion` only after authenticated IPC;
- same-version OpenCodeServerAgent cold-start handling that keeps monitoring
  IPC without unregistering or re-registering because IPC is temporarily
  unavailable;
- direct native Mach-O execution with no shell in the process chain;
- a dedicated OpenCode process group, whole-tree graceful termination, and
  explicit-only force termination;
- strict PID, process start time, executable, process group, user, versioned
  semantic configuration fingerprint, and health validation before crash
  reattachment;
- bounded `1, 2, 5, 15, 30` second recovery attempts;
- authenticated, same-user Unix-domain IPC with 64 KiB message limits;
- `/global/health` checks with optional HTTP Basic Auth;
- native settings for address, port, username, password, mDNS, executable
  selection, and independent login registration;
- progressive password disclosure (stored-value status first, explicit Edit,
  Show, Copy, and Remove actions), config/version restart-pending indicators,
  FDA tri-state display, VoiceOver text, and non-color status labels;
- Unified Logging under subsystem `ai.opencode.server`;
- Xcode-managed arm64 app assembly and outer signing, an explicit Cargo build
  phase for OpenCodeServerAgent and opencodeserverctl, strict bundle validation, mutual
  Designated Requirement checks, and a conservative installation script.

Privacy attribution, Full Disk Access behavior, FileProviderDomain persistence,
and `SMAppService` approval still require the documented clean-machine manual
acceptance run. They cannot be truthfully certified by unit tests or an ad hoc
signature.

## Requirements

- Apple Silicon Mac running macOS 26
- Xcode 26 with the macOS 26 SDK
- stable Rust 1.88 or newer
- Homebrew OpenCode, or another native arm64 Mach-O OpenCode executable
- for Release acceptance builds only: an Apple Development Code Signing
  identity, provided through `~/.config/opencodeserver/signing-identity`
  (see [docs/signing-identity.example.md](docs/signing-identity.example.md));
  every other build path defaults to ad hoc signing

The repository does not create or modify a signing identity automatically.
Creating and trusting a certificate changes the user Keychain and is an
explicit operator action.

## Build

Open `OpenCodeServer.xcodeproj` directly in Xcode 26 and use the shared
`OpenCodeServer` Scheme. The Swift app is a real macOS Application Target;
there is intentionally no parallel `Package.swift` definition.

To inspect the native targets and Scheme:

```sh
xcodebuild -list -project OpenCodeServer.xcodeproj
```

For a Release bundle signed with the configured Apple Development identity
(ADR 0021; the identity name lives in
`~/.config/opencodeserver/signing-identity` and never in the repository):

```sh
./scripts/build.sh
```

For a disposable ad hoc-signed Release bundle:

```sh
SIGNING_IDENTITY=- ./scripts/build.sh
```

`SIGNING_IDENTITY` overrides the default identity for any configuration;
Debug configuration builds default to ad hoc signing.

The wrapper invokes the Xcode Application Target, then copies the complete,
already-signed product to `build/OpenCodeServer.app`. It does not assemble or
modify the bundle after Xcode signing. Set `CONFIGURATION=Debug` for a Debug
bundle and `CLEAN_BUILD=1` for a clean rebuild.

For ad hoc structural verification only, the wrapper asks Xcode’s CodeSign
phase to include the Hardened Runtime option. Ad hoc builds remain unsuitable
for conclusions about TCC persistence, Service Management upgrade identity,
or responsible-code attribution.

Xcode owns Swift/AppKit compilation, the `.app` bundle, generated
`Info.plist`, `LSUIElement`, resources, entitlements, app icon, tests, Scheme,
arm64 deployment settings, Hardened Runtime, and the outer signature. Cargo
continues to own OpenCodeServerAgent and opencodeserverctl. The Xcode Run Script
phase invokes Cargo, copies and signs those two native executables before the outer
application signature, and fails the Xcode build if any step fails.

Xcode’s license must be reviewed and accepted by the local operator before
using Xcode’s normal test runner. The build does not accept it on the user’s
behalf.

## Install

Quit an already installed OpenCodeServer, then:

```sh
./scripts/install.sh build/OpenCodeServer.app
open /Applications/OpenCodeServer.app
```

The installer validates signatures, requires every signed component to carry
the configured Apple Development leaf authority
(`~/.config/opencodeserver/signing-identity`; template:
[docs/signing-identity.example.md](docs/signing-identity.example.md)), and
mutually checks
Designated Requirements against an existing installed version. During the install
transaction the previous bundle is kept only inside the installer's staging
directory: it is restored into place on any failure and deleted after a
successful, finally verified install, leaving no historical app copy. If a
restore itself fails, the staging directory holding the previous bundle is
preserved and its exact path is reported for manual recovery. The installer
does not stop an active OpenCodeServerAgent or OpenCode.

After an installed bundle-version change, OpenCodeServer enters one bounded
OpenCodeServerAgent update transaction through `SMAppService`. Registration API
success is treated only as the start of launch verification: the pending
version and attempt number are persisted separately and become
`RegisteredBundleVersion` only after an authenticated IPC peer proves it is
the pending build by reporting its required build-embedded bundle version.
Each attempt has an adaptive
verification window: 6 x 2 seconds on an interactive system, 15 x 2 seconds
within ten minutes of boot (ADR 0006, 2026-08-03 addendum). A
true bundle update or explicit repair may retry at most twice, for three total
attempts with increasing settling intervals, to contain the documented macOS
26 stale Background Task Management update failure. If OpenCodeServer restarts
while verification is pending, it resumes that attempt and cannot exceed the
same transaction budget. Rejection or exhaustion remains uncommitted and can
be retried on a later OpenCodeServer launch or through the explicit “Repair
OpenCodeServerAgent” command.

At ordinary login, macOS launches OpenCodeServer and OpenCodeServerAgent
independently. Their order is irrelevant. An enabled same-version
OpenCodeServerAgent that is not yet reachable appears as starting or
temporarily unavailable; OpenCodeServer continues monitoring IPC and
never treats that delay as registration corruption.

On first installed launch, OpenCodeServer creates:

```text
~/Library/Application Support/OpenCodeServer/config.plist
~/Library/Application Support/OpenCodeServer/.config-fingerprint.key
~/Library/Application Support/OpenCodeServer/run/control.sock
~/Library/Application Support/OpenCodeServer/run/state.json
```

The configuration, private fingerprint key, grant marker, and runtime state are
mode `0600`; their directories and the socket directory are mode `0700`. At
rest, the password exists only in the login Keychain. When authorized,
OpenCodeServerAgent holds it in memory; while OpenCode is running, it is also
present in the OpenCode child environment. A versioned HMAC-SHA256 tag over
canonical launch semantics is stored in runtime state;
neither the password nor the HMAC key is placed there. Credentials are never
placed in `config.plist`, arguments, status IPC, logs, snapshots, runtime state,
or the signed app.

On OpenCodeServerAgent replacement or crash recovery, process identity is checked before any
configuration comparison. A missing or identity-mismatched PID is treated as a
stale record and is never signaled. One exception is explicit and
conservative: a record whose kernel identity was never confirmed at spawn
(the `identity_unconfirmed` marker) is kept unverified while its PID lives —
never signaled, never taken over, and no second OpenCode is started — and is
discarded only when the PID is provably gone. A matching live process must
then pass the canonical configuration fingerprint, authenticated health, and
a second identity check before reattachment. Runtime state without the current
required fingerprint schema is rejected; there is no metadata fallback or
state migration.

OpenCodeServer is designed for a trusted OpenCode installation, not hostile
native-code containment. Descendants that remain in OpenCode's authorized
process group are managed as a unit; an observed group escape or identity
anomaly fails closed and never authorizes a signal to a foreign group. The
informational installed-version query is single-flight and circuit-breaks
automatic retries after such an anomaly. Deliberate escape after the last
reliable snapshot is an accepted v1 non-goal; see ADR 0015.

## Configuration

The native Settings window and `config.plist` are equal entry points for
non-secret settings. The password is edited in Settings and stored only in the
login Keychain; it is deliberately absent from `config.plist`. Opening Settings
performs only an attribute-only background probe and never decrypts the saved
password. An existing value appears as `Stored in Keychain`; Edit or Copy is
the explicit action that may open the system Keychain prompt, and Remove is
committed only by Save. A default file looks like:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>SchemaVersion</key><integer>1</integer>
  <key>Hostname</key><string>127.0.0.1</string>
  <key>Port</key><integer>4096</integer>
  <key>Username</key><string>opencode</string>
  <key>MDNS</key><false/>
  <key>ExecutablePath</key><string></string>
</dict>
</plist>
```

An empty `ExecutablePath` selects the first valid native arm64 Mach-O from the
standard Homebrew/OpenCode locations and `PATH`. Symbolic links are accepted
and saved as entered, while their final target is validated.

Saving never restarts a running OpenCode. Invalid edits do not affect the
current OpenCode; they block the next OpenCode start with a clear error. When
no credential is stored, leaving the new-password field blank preserves
OpenCode’s native unauthenticated behavior; an existing credential is removed
only through the explicit Remove action. The UI warns before saving an
unauthenticated non-loopback listener but does not override that accepted
product behavior.

## Command-line control

The installed opencodeserverctl executable is:

```text
/Applications/OpenCodeServer.app/Contents/MacOS/opencodeserverctl
```

Commands:

```sh
opencodeserverctl status
opencodeserverctl status --json
opencodeserverctl start
opencodeserverctl stop
opencodeserverctl stop --force
opencodeserverctl restart
opencodeserverctl restart --force
opencodeserverctl logs
opencodeserverctl version
opencodeserverctl validate-config
```

`--force` still starts with the graceful interval and sends `SIGKILL` only after
that interval expires. opencodeserverctl never prints the password.

## Development checks

```sh
xcodebuild -list -project OpenCodeServer.xcodeproj
xcodebuild \
  -project OpenCodeServer.xcodeproj \
  -scheme OpenCodeServer \
  -configuration Debug \
  -derivedDataPath build/XcodeDerivedData \
  -destination 'platform=macOS,arch=arm64' \
  CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=- \
  clean build
xcodebuild \
  -project OpenCodeServer.xcodeproj \
  -scheme OpenCodeServer \
  -configuration Debug \
  -derivedDataPath build/XcodeDerivedData \
  -destination 'platform=macOS,arch=arm64' \
  CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=- ENABLE_HARDENED_RUNTIME=NO \
  test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked --features test-fixture
CLEAN_BUILD=1 CONFIGURATION=Release SIGNING_IDENTITY=- ./scripts/build.sh
./scripts/validate_bundle.sh build/OpenCodeServer.app
```

The hosted AppKit tests set an isolated test mode before application startup,
so they do not create or change the normal Application Support configuration,
register services, or contact a running production OpenCodeServerAgent. Actual keyboard
interaction, Service Management, privacy permissions, and attribution remain
the explicit installed-app checks in the acceptance document.

See [docs/ACCEPTANCE.md](docs/ACCEPTANCE.md) for the manual platform and
privacy gates, and [docs/adr](docs/adr) for the Apple-source-backed decisions.

## License

Released under the [MIT License](LICENSE).
