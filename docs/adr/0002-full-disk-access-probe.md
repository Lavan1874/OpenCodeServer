# ADR 0002: Full Disk Access functional probe

- Status: accepted for v1, versioned acceptance required
- Target: macOS 26

## Decision

OpenCodeServerAgent probes exactly:

```text
~/Library/Safari/History.db
```

It performs only:

1. `open` for read-only access;
2. `fstat` through `File.metadata()`;
3. immediate close.

It never reads file content, scans Safari directories, retains metadata, logs
the protected path, or queries a TCC database.

The result maps to:

- open plus metadata succeeds: `Verified`;
- `EACCES` or `EPERM`: `Not Verified`;
- missing target or any ambiguous failure: `Unable to Determine`.

`Not Verified` means only that this functional probe was denied. It does not
claim that a System Settings switch is definitively off. FDA never blocks
OpenCode startup and never changes the OpenCode health color.

## Rationale and limitation

Apple states that TCC has no general API surface and explicitly recommends
performing the real protected operation and handling errors. Apple also warns
that permission failures can have other causes, including POSIX modes, ACLs,
SIP, Data Vaults, and sandboxing.

Safari history is a minimal read-only v1 probe target, not an undocumented TCC
database heuristic. Its protection and availability must be verified before
and after FDA authorization on every supported macOS release. If the file is
absent, the product reports uncertainty instead of substituting another target.

The exact target is therefore versioned by this ADR and by the clean-state
acceptance checklist. A macOS update that changes its behavior requires a new
ADR and tests.

## Apple sources

- [Reliable test for Full Disk Access? — Apple DTS](https://developer.apple.com/forums/thread/114452)
- [On File System Permissions](https://developer.apple.com/forums/thread/678819)
- [Accessing files from the macOS App Sandbox](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox)
- [Resetting access to protected resources](https://developer.apple.com/documentation/xcode/resetting-access-to-protected-resources-in-macos)

The supported macOS 13+ Full Disk Access settings URL used by the menu is
documented in Apple’s Endpoint Security headers and repeated by Apple DTS:

```text
x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles
```
