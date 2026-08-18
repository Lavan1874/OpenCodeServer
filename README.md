# OpenCodeServer

> **Unofficial project.** OpenCodeServer is an independent third-party
> utility. It is not affiliated with, endorsed by, or supported by the
> OpenCode project (opencode.ai).

[![Tests](https://github.com/Lavan1874/OpenCodeServer/actions/workflows/tests.yml/badge.svg)](https://github.com/Lavan1874/OpenCodeServer/actions/workflows/tests.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-informational.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%2026%2B%20Apple%20Silicon-blue)

OpenCodeServer is a native macOS menu bar utility that keeps one
Homebrew-installed [OpenCode](https://opencode.ai) running — a supervised
background service with status, controls, and settings in the menu bar.

If you run OpenCode's HTTP server on your Mac and want it to survive
restarts, stop cleanly, and be controllable from the menu bar or over SSH
without hand-managing launchd, this is for you.

## What you get

- A menu bar item with live, agent-authoritative OpenCode status (pushed
  over IPC), start / stop / restart actions, and an explicit force-stop
  path with confirmation.
- A native Settings window for address, port, username, password, mDNS,
  OpenCode executable selection, and independent login registration for
  the app and its background agent.
- Credentials stored only in the login Keychain — never in config files,
  arguments, logs, status IPC, or the signed app.
- `opencodeserverctl`, a small native command-line client speaking the
  same authenticated IPC, for local and SSH administration.
- Crash recovery with bounded backoff, graceful whole-process-tree
  shutdown, and explicit-only forced termination.
- Privacy-conservative by design: not App Sandbox-enabled (its purpose is
  launching developer tooling for your projects) but it never touches
  firewall rules, TCC databases, OpenCode configuration, projects,
  providers, models, sessions, or plugins.

## How it works

OpenCodeServer is one standard macOS `.app` with two long-running native
components:

- **OpenCodeServer** — the Swift/AppKit menu bar GUI and settings editor;
- **OpenCodeServerAgent** — a small Rust supervisor managed by launchd
  through `SMAppService`, the only OpenCode process manager and the only
  authority on OpenCode runtime state.

Both register independently at login. Quitting the menu bar app never
stops OpenCode. Supervision, the bounded Service Management update
transaction, IPC authentication, credential handling, and the on-disk
state model are described in
[docs/architecture.md](docs/architecture.md); the reasoning behind each
decision lives in the [ADRs](docs/adr).

## Requirements

- Apple Silicon Mac running macOS 26
- Homebrew OpenCode, or another native arm64 Mach-O OpenCode executable
- To build from source: Xcode 26 with the macOS 26 SDK, and stable Rust
  1.88 or newer

## Download & install

Prebuilt bundles for every release are on the
[GitHub Releases](https://github.com/Lavan1874/OpenCodeServer/releases)
page. They are signed with an Apple Development certificate and **not
notarized** — the Gatekeeper friction differs per channel, so pick what
suits you:

**A. Command line — no Gatekeeper prompt.** `curl` downloads carry no
quarantine attribute, so the app opens without any dialog. Extract with
`ditto` (not `unzip`) to preserve the signature:

```sh
curl -LO https://github.com/Lavan1874/OpenCodeServer/releases/latest/download/OpenCodeServer-latest.zip
ditto -x -k OpenCodeServer-latest.zip
mv OpenCodeServer.app /Applications
open /Applications/OpenCodeServer.app
```

**B. Browser download.** Grab the `.zip` or `.dmg` from the Releases
page and drag `OpenCodeServer.app` to /Applications. The first open is
blocked because the app is not notarized; approve it once via System
Settings → Privacy & Security → **"Open Anyway"**. This repeats after
every version update.

**C. Homebrew tap.**

```sh
brew tap lavan1874/opencodeserver
brew trust lavan1874/opencodeserver
brew install --cask opencodeserver
```

`brew trust` is a one-time opt-in current Homebrew requires before
loading packages from a third-party tap. Homebrew quarantines cask
downloads, so expect the same one-time "Open Anyway" approval after each
install and each `brew upgrade --cask`.

Managed (MDM/enterprise) Macs may refuse non-notarized apps entirely,
regardless of channel. First launch registers the background
OpenCodeServerAgent through `SMAppService` and creates the configuration
under `~/Library/Application Support/OpenCodeServer`.

Releasing is automated by `scripts/cut-release.sh <notes.md>` (clean
Release build with the configured identity, ditto zip + dmg with zip
round-trip signature verification, GitHub Release with a stable
`-latest.zip` alias, and a Homebrew tap notification that auto-bumps the
cask).

## Build

Open `OpenCodeServer.xcodeproj` directly in Xcode 26 and use the shared
`OpenCodeServer` Scheme, or build from the command line. Debug builds and
CI use ad hoc signing; there is intentionally no parallel `Package.swift`.

For a disposable ad hoc-signed Release bundle:

```sh
SIGNING_IDENTITY=- ./scripts/build.sh
```

`SIGNING_IDENTITY` overrides the signing identity for any configuration;
`CONFIGURATION=Debug` and `CLEAN_BUILD=1` are also honored. The wrapper
invokes the Xcode Application Target, then copies the complete,
already-signed product to `build/OpenCodeServer.app` — it never assembles
or modifies the bundle after Xcode signing.

Maintainers running Release acceptance builds provide a real Apple
Development identity through `~/.config/opencodeserver/signing-identity`
(see [docs/signing-identity.example.md](docs/signing-identity.example.md));
that path matters only for privacy/TCC acceptance, not for building or
testing.

## Install

Ad hoc builds are for local experimentation — copy the built app where you
want it, or run it in place. The transactional installer is the maintainer
Release path (it enforces a configured signing authority):

```sh
./scripts/install.sh build/OpenCodeServer.app
open /Applications/OpenCodeServer.app
```

The installer validates signatures and Designated Requirements, stages the
candidate, and atomically replaces the previous bundle — restored on any
failure, with the staging path reported if a restore itself fails. It does
not stop an active OpenCodeServerAgent or OpenCode. After a bundle-version
change the app runs one bounded registration-update transaction before
committing the new version; see
[docs/architecture.md](docs/architecture.md).

On first launch OpenCodeServer creates:

```text
~/Library/Application Support/OpenCodeServer/config.plist
~/Library/Application Support/OpenCodeServer/.config-fingerprint.key
~/Library/Application Support/OpenCodeServer/run/control.sock
~/Library/Application Support/OpenCodeServer/run/state.json
```

Configuration, the fingerprint key, and runtime state are mode `0600`
under mode-`0700` directories; the socket directory is `0700` as well.
The password exists at rest only in the login Keychain.

## Configuration

The native Settings window and `config.plist` are equal entry points for
non-secret settings; the password is edited in Settings and stored only in
the Keychain, and is deliberately absent from `config.plist`. A default
file looks like:

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

An empty `ExecutablePath` selects the first valid native arm64 Mach-O
from the standard Homebrew/OpenCode locations and `PATH`. Symbolic links
are accepted and saved as entered; their final target is validated.

Saving never restarts a running OpenCode. Invalid edits do not affect the
current OpenCode; they block the next start with a clear error. With no
credential stored, a blank password preserves OpenCode's native
unauthenticated behavior; an existing credential is removed only through
the explicit Remove action. The UI warns before saving an unauthenticated
non-loopback listener but does not override that accepted behavior.

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

`--force` still starts with the graceful interval and sends `SIGKILL` only
after that interval expires. opencodeserverctl never prints the password.

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

The hosted AppKit tests set an isolated test mode before application
startup, so they do not create or change the normal Application Support
configuration, register services, or contact a running production
OpenCodeServerAgent. Real keyboard interaction, Service Management,
privacy permissions, and attribution are covered by the manual gates in
[docs/ACCEPTANCE.md](docs/ACCEPTANCE.md).

## License

Released under the [MIT License](LICENSE).
