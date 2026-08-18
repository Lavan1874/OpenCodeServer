# ADR 0001: Native bundle and Service Management

- Status: accepted
- Target: macOS 26, Apple Silicon

## Decision

OpenCodeServer is a standard `LSUIElement` app bundle. Its long-running product
components are the Swift/AppKit OpenCodeServer executable and the Rust
OpenCodeServerAgent. The native `opencodeserverctl` executable is an auxiliary
control client, not a
third long-running component.

OpenCodeServerAgent is shipped as an app-like nested helper bundle so macOS can
resolve its stable user-visible identity when the managed OpenCode performs
Bonjour operations:

```text
Contents/Resources/OpenCodeServerAgent.app/Contents/Info.plist
Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent
```

The OpenCodeServerAgent property list lives at:

```text
Contents/Library/LaunchAgents/ai.opencode.server.agent.plist
```

It uses:

```text
BundleProgram = Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent
AssociatedBundleIdentifiers = [ai.opencode.server]
```

The app-like helper bundle removes the old bare-executable fallback from the
release layout, but it does not by itself prove that every TCC surface will
display the main-app name. Build 64 VM evidence showed the interactive Local
Network prompt as `OpenCodeServer`, while the System Settings Local Network
list still displayed `OpenCodeServerAgent`. Both surfaces require clean-state
verification before this attribution decision can be considered closed.

The UI registers it with:

```swift
SMAppService.agent(plistName: "ai.opencode.server.agent.plist")
```

OpenCodeServer uses `SMAppService.mainApp` independently. The product
never copies its production plist into `~/Library/LaunchAgents`.

An updated OpenCodeServerAgent or plist is unregistered and re-registered when
OpenCodeServer first runs after a bundle-version change. The running OpenCode
process is left untouched and the new OpenCodeServerAgent uses strict
reattachment checks. Registration acceptance is not treated as proof of
OpenCodeServerAgent execution; the persistent, IPC-verified update state
machine is specified in ADR 0006 and its login-startup boundary in ADR 0008.

## Rationale

Apple documents `SMAppService` as the supported API for helpers inside a main
bundle and as the replacement for manually installing LaunchAgent plists.
Apple’s update guidance requires a bundle-relative `BundleProgram`, places the
plist in `Contents/Library/LaunchAgents`, and notes that changed helper content
must be re-registered. TN3179 and Apple DTS guidance further recommend an
app-like bundle for a launchd agent whose network activity needs a stable
responsible-code identity. A bare Mach-O with an embedded `Info.plist` was
observed in the macOS 26 VM to fall back to the truncated process name
`OpenCodeServerA`. The nested bundle corrected the interactive prompt in the
Build 64 VM test, but the corresponding System Settings attribution still
requires a separate clean-state decision.

## Apple sources

- [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Registering a service](https://developer.apple.com/documentation/servicemanagement/smappservice/register())
- [Updating helper executables](https://developer.apple.com/documentation/servicemanagement/updating-helper-executables-from-earlier-versions-of-macos)
- [Managing ongoing background processes](https://developer.apple.com/documentation/appkit/managing-ongoing-background-processes-in-your-mac)
- [Placing content in a bundle](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle)
- [On File System Permissions](https://developer.apple.com/forums/thread/678819)
- [TN3179: Understanding local network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
- [Signing a daemon with a restricted entitlement](https://developer.apple.com/documentation/xcode/signing-a-daemon-with-a-restricted-entitlement)

## Consequences

- Registration and approval are per user.
- Disabling the background item in System Settings is authoritative.
- `SMAppService.Status` is the UI’s registration-state source.
- Manual bootstrap with `launchctl` is not a supported production path.
- Privacy attribution is an acceptance-tested platform behavior, not inferred
  merely from the plist.
