# ADR 0005: Stable configuration fingerprint and reattachment ordering

- Status: accepted
- Target: macOS 26, Apple Silicon

## Context

Runtime state originally persisted the `st_dev`, inode, modification time, and
length of `config.plist` and compared that tuple before checking whether the
recorded PID still represented the same live OpenCode process.

`st_dev` is the device identifier of the current mount, not a documented
persistent volume identity. A normal macOS restart can change it while inode,
modification time, length, and configuration content remain unchanged. The old
ordering consequently classified a dead, stale PID as an existing process with
changed configuration and blocked automatic startup.

Metadata is also not a content identity: atomic replacement can change inode
and time without changing semantics, while same-length content can differ.

## Decision

Reattachment is ordered as follows:

1. Inspect the persisted PID and compare PID, dedicated process group, effective
   user, process start seconds and microseconds, and executable path.
2. A missing PID or mismatched identity is a stale record. Clear it without
   signaling anything and continue the desired-state startup path, whose port
   preflight still prevents duplicate listeners.
3. An inspection error is uncertainty. Preserve the record, report failure, and
   neither signal nor start a second OpenCode.
4. For the same live process, verify a versioned HMAC-SHA256 tag over canonical
   launch semantics: schema, hostname, port, effective username, password, mDNS,
   and the configured executable path. A fingerprint mismatch no longer strands
   the process (amended 2026-08-05, see below).
5. Require an authenticated healthy `/global/health` response.
6. Recheck the complete process identity after health verification and attach
   only if it is still the same process.

### Configuration drift on an identity-verified process (2026-08-05 amendment)

Steps 1–3 answer "is this the process we started?" — a question of kernel
identity. Step 4 answers "does it run the current configuration?" — a question
of freshness. Originally a freshness mismatch was treated like failed identity:
the process was left unmanaged, unsignalable, and holding the endpoint, which
deadlocked the product (measured 2026-08-05: a password change without an
OpenCode restart, followed by an agent re-registration, made Start impossible
and Repair ineffective; only a manual kill recovered).

A fingerprint mismatch on an identity-verified process now ADOPTS the process
as a stale-configuration child: identity evidence, not freshness, is what
authorizes later signals, and stop-time identity revalidation still guards
every signal. The adopted process keeps supervision (Stop/Restart available,
no second OpenCode, exit watched), is reported as `config_pending`, and a
revert of the change (or a credential that becomes readable) upgrades the
attachment in place after the same identity + health re-verification.
"Restart OpenCode" is the documented convergence path and the Settings save
flow offers it immediately.

The HMAC key is 32 bytes from the operating system random source and is stored
at:

```text
~/Library/Application Support/OpenCodeServer/.config-fingerprint.key
```

It must be a user-owned regular file with mode `0600`. It is opened with
path-versus-file-descriptor identity validation. The key, password, and
canonical configuration are never placed in runtime state, logs, status IPC, or
errors. Fingerprint debug formatting redacts the tag.

Every process record requires `config_fingerprint`. `config_stamp`, optional
fingerprints, metadata comparison, and migration branches have been removed;
only the current runtime-state schema is accepted.

The `lstat`/`fstat` device-and-inode comparison during one configuration or key
read remains in place. It prevents path replacement races and is separate from
persistent semantic equality.

## Security rationale

- No process is ever signaled merely because its PID matches.
- PID reuse is rejected by start time, process group, user, and executable.
- A stale record cannot block startup due to unrelated configuration metadata.
- An unverifiable live process remains untouched and blocks a duplicate.
- HMAC avoids storing a direct offline password digest in state.
- Length-prefixed, fixed-order canonical encoding avoids ambiguity and ignores
  plist formatting or key ordering.
- The post-health identity check closes the reattachment check/use window.

The configuration stored its password in a private plist when this ADR was
written. ADR 0016 later moved the password to the login keychain; the
fingerprint still covers the (now keychain-sourced) merged password.

## Dependencies

`hmac` and `sha2` provide standard HMAC-SHA256 without project-authored unsafe
code. `getrandom` obtains the per-install key from the operating system random
source. These narrowly scoped dependencies replace a metadata heuristic that
cannot provide the required semantics.

## Apple sources

- [stat(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/stat.2.html)
- [Persistent volume UUID](https://developer.apple.com/documentation/foundation/urlresourcevalues/volumeuuidstring)
- [On File System Permissions](https://developer.apple.com/forums/thread/678819)
