# ADR 0012: The OpenCodeServerAgent LaunchAgent runs as ProcessType Interactive

## Status

Accepted (build 14); settled product decision by the product owner
(Build 16 rework directive, 2026-08-01). `Interactive` stays; it must not be
reverted to `Background`, `Standard`, or an unset value.

## Context

Cold-boot measurements (2026-07-31/08-01, four real reboots) showed the menu
bar icon needed 21–37s to turn green. The dominant segment was
OpenCodeServerAgent's launch trampoline: 12.7–18.9s between launchd spawning
`xpcproxy` and the OpenCodeServerAgent binary actually executing, while every
other third-party agent on the same machine — SMAppService-registered
(Karabiner, Secretive, Mac Mouse Fix, PasteNow) or legacy plist (hermes, Kimi,
UU) — executed in 0.08–0.42s of the same login batch.

A root `sample` of the trampoline showed all time spent inside platform code
(dyld bootstrap, XPCSupport dlopen, BTM path resolution, `posix_spawn`,
`getpwuid`), with every generic syscall stretched uniformly — the signature of
priority starvation, not a single blocking call.

The OpenCodeServerAgent plist declared `ProcessType = Background`. Per `man
launchd.plist`, Background jobs receive resource limits "intended to prevent
them from disrupting the user experience" — the heaviest of the four classes —
while unset/Standard jobs get only light limits and Interactive jobs run with
no limits, same as apps. OpenCodeServerAgent was the only Background-throttled
job in the comparison set; the fast agents run Interactive (Mac Mouse Fix, UU)
or unset/Standard (hermes, Kimi). Under the login storm the trampoline's every
syscall and IPC queued at the lowest priority; on an idle system (hot restart)
the same path completes in ~1s.

Non-authoritative anecdote only (not a decision basis):
github.com/openclaw/openclaw/issues/58061 measured a launchd-started process
going from minutes to 4s after setting `ProcessType = Interactive`. The
decision rests on the cold-boot measurements above and on `man
launchd.plist`, not on third-party reports.

## Decision

Set `ProcessType = Interactive` in `resources/ai.opencode.server.agent.plist`.
This is a settled product decision by the product owner, not an experiment:
it must not be reverted to `Background`, `Standard`, or an unset value.

Apple's documented criterion for Interactive is that "an app's ability to be
responsive depends on it, and cannot be made Adaptive". OpenCodeServer is a
pure status display of OpenCodeServerAgent, so its perceived responsiveness
depends entirely on OpenCodeServerAgent and its managed OpenCode. Adaptive is
not applicable: its activity signal is XPC transactions, and
OpenCodeServerAgent's IPC is a Unix domain socket, so the job would sit in
the Background band when idle.

`scripts/validate_bundle.sh` now fails the build if the OpenCodeServerAgent
plist does not declare `ProcessType = Interactive`.

## Consequences

- Cold-boot effect, kept under continuing real-machine observation:
  trampoline 13–19s is expected to drop below a second, with icon → green
  limited mostly by OpenCode's own cold start. Continued observation monitors
  for regressions; it does not reopen the settled decision.
- OpenCodeServerAgent and its process group (including OpenCode) get app-level
  scheduling during the login storm. This is a deliberate trade: the service
  is latency-critical user-facing infrastructure, not discretionary
  background work.
- `Background` must not be reintroduced as a "good citizen" gesture; the
  build-time check in `validate_bundle.sh` guards the regression.
- Exact per-class throttling numbers are not publicly documented by Apple;
  the Standard-vs-Background severity gradient is an empirical reading of
  `man launchd.plist`, not an Apple guarantee. Continuing cold-boot
  measurements and acceptance of resource usage and the
  OpenCodeServerAgent/OpenCode lifecycle watch for regressions; they do not
  reopen the decision.
