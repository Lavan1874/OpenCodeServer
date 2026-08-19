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

## 2026-08-20 amendment: per-OS probe gate and existence-aware consensus

Status: accepted. Supersedes the single-target decision above for
OpenCodeServerAgent builds after Build 86; the operation constraints
(open + metadata + close, no content read, no TCC database queries) are
unchanged.

### Measured background (2026-08-18 through 2026-08-20)

- macOS 26.6.1 (25G76), clean VM A/B with Terminal.app FDA as the only
  variable: without FDA every classic protected path denies open/listing
  while `stat` succeeds for every path; with FDA every path opens. The
  observed denial errno is `EPERM`, so the existing `EPERM` arm of the
  classifier is load-bearing, not dead code.
- Maintainer host (26.6.1): in one no-FDA context exactly
  `~/Library/Safari/History.db`, `~/Library/Messages/chat.db`, and the
  user `TCC.db` became readable while sibling protected directories
  stayed denied. The drift is host-local (a clean VM denies all three)
  and its exact cause was not determined; the affected files carried
  `com.apple.provenance` xattrs and fine-grained
  `kTCCServiceSystemPolicyAppData` rows existed on that machine. A
  single-file probe can therefore report a false `Verified` on drifted
  machines.
- macOS 27.0 beta (26A5416b), no-FDA clean process contexts: every
  classic FDA-protected path that exists — fifteen measured, including
  Calendar, AddressBook, `MobileSync/Backup`, HomeKit, and all
  `~/Library/CloudStorage` roots — is readable without FDA. The
  user-level TCC store moved to a ProtectedSystem container that is
  unreadable even with FDA. Browser Application Support protection is
  real but XProtect-list-driven and measurably non-deterministic per
  machine (policy delivered yet unenforced on our test machine,
  enforced on a third-party researcher's). Granting FDA to a terminal
  changed no observable outcome.
- Case study, tw93/Mole: 1.6.2 detected FDA by reading the user TCC
  database and produced false negatives on 27 beta (issue #1089);
  1.7.0 checks `stat` on any of three paths and returns "granted"
  regardless of the FDA state — verified by running Mole's function
  verbatim in a clean-VM A/B on 26.6.1 and on the 27 beta host. Both
  failure directions (always-no, always-yes) motivate this amendment.

### Decision

1. The probe is version-gated by the major version of
   `sysctl kern.osproductversion`. Only macOS 26.x probes. Every other
   value — including macOS ≥27 and an unreadable version — returns
   `Unable to Determine` without touching the filesystem. On 27 beta no
   measured path discriminates the FDA state, so a `Verified` claim
   there would be a heuristic, not an observed fact.
2. On macOS 26 the single target is replaced by an existence-aware
   consensus over three versioned targets:
   `~/Library/Safari/History.db`, `~/Library/Mail/V10`, and
   `~/Library/Suggestions`. `stat` may be used for existence only — it
   is not TCC-gated and must never be the access test. Per target the
   access test remains open + metadata + close with no content read.
   - all existing targets accessible → `Verified`;
   - all existing targets denied (`EACCES` or `EPERM`) → `Not Verified`;
   - mixed outcomes, any ambiguous failure, or no target exists →
     `Unable to Determine`.
   Consensus exists because of the measured single-file drift: a drifted
   target degrades the result to `Unable to Determine` instead of a
   false `Verified`.
3. macOS 27 GA re-measurement gate: before any 27 support decision,
   re-run the clean-state matrix (clean UTM VM recommended, with and
   without Terminal.app FDA). If a path is documented that denies
   without FDA and opens with FDA, add a 27 row to the target table
   through a new dated amendment with versioned tests. The maintainer's
   working expectation is that Apple will keep FDA meaningful in the
   27 release line; that is a hypothesis to re-measure, never an
   assumption encoded in the probe.

### Amendment sources

- Apple, macOS 27 release notes — TCC deprecation (90775556).
- Wojciech Reguła, "Crossing the Golden Gate: macOS's New Application
  Support Protection" (2026-07).
- Howard Oakley, "What just changed in XProtect?" (2026-08-07).
- tw93/Mole issue #1089 and the Mole source (`lib/core/ui.sh`,
  `has_full_disk_access`, 1.7.0).
- On-metal measurement logs, 2026-08-18 through 2026-08-20: maintainer
  host 26.6.1, clean VMs 26.6.1 (FDA granted / not granted), and a
  27.0 beta (26A5416b) real machine.

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
