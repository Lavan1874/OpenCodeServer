# ADR 0004: Native Xcode macOS Application project

- Status: accepted
- Target: Xcode 26, macOS 26, Apple Silicon

## Context

The first v1 implementation compiled the Swift/AppKit UI as a SwiftPM
executable and used shell scripts to construct, populate, and sign
`OpenCodeServer.app`. That produced a valid prototype bundle, but it duplicated
responsibilities that a macOS Application Target is designed to own and left
Xcode unable to manage the app lifecycle, resources, tests, build phases, and
archive as one native product.

The migration must not rewrite the product architecture. The AppKit menu bar
OpenCodeServer, OpenCodeServerAgent, opencodeserverctl, embedded LaunchAgent,
identifiers, Service Management
registration, configuration, IPC, and runtime behavior remain unchanged.

## Decision

`OpenCodeServer.xcodeproj` is the only Swift build definition. It contains:

- a `com.apple.product-type.application` macOS Application Target;
- a hosted XCTest Target;
- a shared `OpenCodeServer` Scheme;
- Debug and Release configurations based on `Config/*.xcconfig`;
- generated application `Info.plist` metadata, including `LSUIElement`;
- an asset catalog and Xcode-managed app icon;
- an empty, minimal app entitlement file with App Sandbox disabled;
- an Xcode Copy Files phase for the LaunchAgent property list; and
- an Xcode Run Script phase for the two Cargo products.

The Swift entry point follows the Xcode 26 AppKit template pattern:
`@main` is applied to `AppDelegate`, which remains an
`NSApplicationDelegate`. The Main storyboard contains only the Application and
Main Menu scene. The settings window stays programmatic AppKit because moving
it into Interface Builder would be an unrelated UI rewrite.

The Main storyboard provides the standard Application and Edit menus plus a
minimal Window menu applicable to the non-resizable Settings window. Edit
commands use standard selectors with a nil target at runtime. AppKit therefore
routes Undo, Redo, Cut, Copy, Paste, Paste and Match Style, Delete, and Select
All through the Responder Chain to the active native field editor. No field
uses a custom keyboard interceptor or clipboard implementation to emulate
these commands.

## Cargo integration

Cargo remains authoritative for `OpenCodeServerAgent` and
`opencodeserverctl`. The Application Target’s Run Script phase invokes
`scripts/xcode-build-rust.sh`. Its declared inputs and outputs live in
versioned `.xcfilelist` files.

The phase:

1. selects the Cargo profile matching Xcode’s Debug or Release configuration;
2. builds both binaries with `--locked`;
3. refuses a non-arm64 build host or non-arm64 output;
4. copies the fresh products to `Contents/MacOS`;
5. signs OpenCodeServerAgent and opencodeserverctl with their stable
   identifiers and Hardened Runtime; and
6. fails the Xcode build on any Cargo, copy, architecture, signing, or
   verification failure.

The phase runs on every app build because the effective signing identity is
not an ordinary source-file dependency and stale nested signatures are not
acceptable. Cargo’s own dependency graph and target directory preserve
incremental compilation. The phase does not read user configuration or place
credentials in the build environment.

Xcode user-script sandboxing is disabled for this phase. Cargo and the selected
Rust installation legitimately use toolchain and registry caches outside the
project/Derived Data directories, and Xcode has no native Rust compiler rule
that can replace that access. This is the only non-template build-setting
exception required by the integration.

## Bundle assembly and signing

Xcode owns the application wrapper, compiled storyboard, asset catalog,
generated metadata, resources, `PkgInfo`, Swift executable, outer signature,
and archive. The LaunchAgent plist is copied by an Xcode Copy Files phase to:

```text
Contents/Library/LaunchAgents/ai.opencode.server.agent.plist
```

The Cargo phase finishes and signs nested code before Xcode’s final CodeSign
step seals the outer app. `scripts/build.sh` is now only an auditable
`xcodebuild` wrapper. It may copy the complete signed product to
`build/OpenCodeServer.app`, but it does not add, remove, replace, or re-sign
anything inside that bundle.

For an ad hoc structural build, the wrapper supplies the runtime option to
Xcode’s own CodeSign phase because Xcode otherwise intentionally omits
Hardened Runtime for “Sign to Run Locally.” Stable-identity integration and
privacy acceptance use the normal `ENABLE_HARDENED_RUNTIME` setting and remain
mandatory before release.

Nested timestamping defaults off so the designated local self-signed identity
can build without a public timestamp service. A future Developer ID release
sets `OPENCODESERVER_CODE_SIGN_TIMESTAMP=1` for the Cargo phase and must
validate timestamping for every nested executable and the outer archive.

## SwiftPM disposition

`Package.swift` and the standalone `main.swift` launcher were removed after
the Application and XCTest Targets successfully replaced their build and test
roles. Keeping both definitions would allow source membership, settings,
resources, and lifecycle behavior to drift.

## Consequences

- Xcode 26 can load, build, test, analyze, and archive the app as a native
  macOS product.
- The Xcode project is human-reviewable and does not require XcodeGen, Tuist,
  or another generator.
- AppKit, `NSStatusItem`, `NSMenu`, `NSWindowController`, `LSUIElement`, and
  `SMAppService.agent(plistName:)` remain in place.
- Cargo and Xcode each retain one clear build responsibility.
- Actual keyboard event handling, Service Management approval, TCC/FDA,
  FileProviderDomain attribution, stable Designated Requirement continuity,
  and installation under `/Applications` remain installed-app acceptance
  tests; unit tests do not claim to prove them.

## Apple sources

- [Configuring a new target](https://developer.apple.com/documentation/xcode/configuring-a-new-target-in-your-project/)
- [Customizing target build phases](https://developer.apple.com/documentation/xcode/customizing-the-build-phases-of-a-target)
- [Running custom scripts during a build](https://developer.apple.com/documentation/xcode/running-custom-scripts-during-a-build)
- [Improving incremental builds](https://developer.apple.com/documentation/xcode/improving-the-speed-of-incremental-builds)
- [Customizing build schemes](https://developer.apple.com/documentation/xcode/customizing-the-build-schemes-for-a-project)
- [Running tests and interpreting results](https://developer.apple.com/documentation/xcode/running-tests-and-interpreting-results)
- [Configuring an app icon](https://developer.apple.com/documentation/xcode/configuring-your-app-icon/)
- [Configuring Hardened Runtime](https://developer.apple.com/documentation/xcode/configuring-the-hardened-runtime)
- [Creating distribution-signed code for the Mac](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/)
- [Enabling menu items and the Responder Chain](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/MenuList/Articles/EnablingMenuItems.html)
- [Placing content in a bundle](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle)
- [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
