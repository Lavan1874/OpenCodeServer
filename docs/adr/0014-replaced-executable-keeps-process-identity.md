# ADR 0014: A replaced executable file does not change a process's identity

## Status

Accepted (build 16)

## Context

On 2026-08-01 Homebrew upgraded OpenCode 1.18.8 to 1.18.10 and deleted the
old keg while OpenCode PID 653, started from that keg, kept running and
serving. From then on `proc_pidpath` for that PID fails with `ENOENT` while
`proc_pidinfo` still returns the full kernel identity. Apple documents no
errno semantics for `proc_pidpath`; the `ENOENT` mapping is empirically
verified (integration test
`snapshot_reports_no_path_for_a_deleted_executable_file`, plus this incident).

Build 15 mapped any snapshot error to "inspection failed": every poll
surfaced `OpenCode process inspection failed: No such file or directory
(os error 2)` in the GUI between health checks, Stop/Restart were refused
("Refused to stop OpenCode because process identity validation failed"), and
an agent restart could not reattach ("process identity could not be
inspected"). The healthy service became unmanageable until the process was
killed externally.

The executable-path check exists to guard against PID reuse. But a reused
PID already fails the kernel fields: it necessarily has a different start
timestamp. When pid, process-group leadership, effective uid, and
start time all match, the process is the recorded one whether or not its
executable file still exists on disk.

## Decision

- `ProcessSnapshot.executable` becomes `Option<PathBuf>`: `None` only when
  `proc_pidinfo` succeeded and `proc_pidpath` failed with `ENOENT`. Any
  other probe error keeps failing the snapshot, and `ESRCH` still means the
  process is gone.
- Identity comparison becomes a three-way `identity_probe`:
  `Match` / `ExecutableVanished` / `Mismatch`. Kernel fields are checked
  first; only then the path: known and equal → `Match`; known and
  different → `Mismatch` (possible PID reuse — signaling stays refused);
  unknown (`None`) → `ExecutableVanished`.
- `ExecutableVanished` is accepted wherever identity is required: the signal
  gate, the attached-process exit poll, and reattachment
  (`RecordIdentity::ExecutableVanished`, logged once at reattach). A missing
  file never weakens the kernel fields — a mismatched start time with a
  `None` path is still a `Mismatch`.
- Spawn-time `wait_for_snapshot` gets stricter, not looser: a fresh spawn
  whose executable path is already unknown is rejected.

## Consequences

- A Homebrew upgrade no longer wedges management of a running OpenCode:
  Stop/Restart keep working and the GUI stops flickering the inspection
  error.
- Restart after an upgrade re-resolves the configured path and launches the
  new version, clearing `version_pending`.
- The protection against signaling a reused PID is unchanged: a path
  mismatch or any kernel-field mismatch still refuses.
- Reattachment still requires a valid current configuration (ADR 0005
  fingerprint); if the configured executable path itself no longer resolves
  (OpenCode removed without replacement), the record stays unverified by
  design.
- Tests: unit probes for the three classifications, including "a vanished
  executable does not rescue a reused PID"; integration tests for stop,
  reattach, and restart-after-upgrade — all verified to fail on build 15.
