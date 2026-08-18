#!/bin/zsh
# capture_registration_logs.sh — diagnostic harness for the first-registration
# no-spawn investigation (fix/first-registration-spawn). Not part of the product.
#
# Captures, for a bounded window (default 90s):
#   - `log stream` for launchd and backgroundtaskmanagementd (info level)
#   - `launchctl print gui/<uid>/ai.opencode.server.agent` snapshots every 2s
#   - `sfltool dumpbtm` before and after (30s watchdog; it has hung before)
#
# Usage: scripts/capture_registration_logs.sh [duration_seconds]
set -uo pipefail

duration="${1:-90}"
script_dir="${0:A:h}"
repo_root="${script_dir:h}"
stamp="$(date +%Y%m%d-%H%M%S)"
out_dir="$repo_root/build/captures/$stamp"
mkdir -p "$out_dir"

label="ai.opencode.server.agent"
uid="$(id -u)"

print "capture dir: $out_dir"
print "duration: ${duration}s"

# BTM dump before (best-effort): sfltool dumpbtm currently hangs on this
# machine (same symptom ADR 0006 recorded during BTM poisoning), so it runs
# fully detached with a hard SIGKILL watchdog and never blocks the capture.
( sfltool dumpbtm > "$out_dir/btm-before.txt" 2>&1 & 
  dump_pid=$!
  ( sleep 20; kill -9 "$dump_pid" 2>/dev/null ) & 
  wait "$dump_pid" 2>/dev/null ) &

/usr/bin/log stream --process launchd --level info \
  > "$out_dir/launchd.stream.txt" 2>&1 &
launchd_stream_pid=$!

/usr/bin/log stream --process backgroundtaskmanagementd --level info \
  > "$out_dir/btm.stream.txt" 2>&1 &
btm_stream_pid=$!

# smd is the ServiceManagement daemon that submits SMAppService registrations
# ("submitted by smd" in launchctl print); launchd itself stays silent here.
/usr/bin/log stream --process smd --level info \
  > "$out_dir/smd.stream.txt" 2>&1 &
smd_stream_pid=$!

cleanup() {
  kill "$launchd_stream_pid" "$btm_stream_pid" "$smd_stream_pid" 2>/dev/null
  ( sfltool dumpbtm > "$out_dir/btm-after.txt" 2>&1 &
    dump_pid=$!
    ( sleep 20; kill -9 "$dump_pid" 2>/dev/null ) &
    wait "$dump_pid" 2>/dev/null ) &
  print "capture finished: $out_dir"
}
trap cleanup EXIT INT TERM

# launchctl snapshots every 2s.
end=$(( SECONDS + duration ))
while (( SECONDS < end )); do
  {
    print "===== $(date +%H:%M:%S.%N) ====="
    launchctl print "gui/$uid/$label" 2>&1
  } >> "$out_dir/launchctl.snapshots.txt"
  sleep 2
done

exit 0
