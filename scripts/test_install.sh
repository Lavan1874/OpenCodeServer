#!/bin/zsh
set -euo pipefail

if (( $# != 2 )); then
    print -u2 "usage: test_install.sh <previous OpenCodeServer.app> <candidate OpenCodeServer.app>"
    exit 2
fi

repo_root="${0:A:h:h}"
previous="${1:A}"
candidate="${2:A}"
test_root="$(/usr/bin/mktemp -d /private/tmp/OpenCodeServer-install-tests.XXXXXXXX)"

cleanup() {
    if [[ -n "$test_root" \
        && "$test_root" == /private/tmp/OpenCodeServer-install-tests.* \
        && -d "$test_root" \
        && ! -L "$test_root" ]]
    then
        /bin/rm -R "$test_root"
    fi
}
trap cleanup EXIT

bundle_version() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$1/Contents/Info.plist"
}

assert_no_staging() {
    local applications_dir="$1"
    local staging=("$applications_dir"/.OpenCodeServer.install.*(N))
    if (( ${#staging} != 0 )); then
        print -u2 "staging directory was not cleaned: $applications_dir"
        exit 1
    fi
}

run_installer() {
    local applications_dir="$1"
    local failure="${2:-}"
    OPENCODESERVER_INSTALL_TESTING=1 \
    OPENCODESERVER_INSTALL_APPLICATIONS_DIR="$applications_dir" \
    OPENCODESERVER_INSTALL_TEST_FAILURE="$failure" \
        "$repo_root/scripts/install.sh" "$candidate"
}

typeset -g installer_pid=""

run_installer_background() {
    local applications_dir="$1"
    local failure="$2"
    local log_file="$3"
    OPENCODESERVER_INSTALL_TESTING=1 \
    OPENCODESERVER_INSTALL_APPLICATIONS_DIR="$applications_dir" \
    OPENCODESERVER_INSTALL_TEST_FAILURE="$failure" \
        "$repo_root/scripts/install.sh" "$candidate" >"$log_file" 2>&1 &
    installer_pid=$!
}

new_applications_dir() {
    local label="$1"
    local directory="$test_root/$label/Applications"
    /bin/mkdir -p "$directory"
    print -r -- "$directory"
}

candidate_version="$(bundle_version "$candidate")"
previous_version="$(bundle_version "$previous")"

first_install_dir="$(new_applications_dir first-install)"
run_installer "$first_install_dir"
[[ "$(bundle_version "$first_install_dir/OpenCodeServer.app")" == "$candidate_version" ]]
assert_no_staging "$first_install_dir"

upgrade_dir="$(new_applications_dir normal-upgrade)"
/usr/bin/ditto "$previous" "$upgrade_dir/OpenCodeServer.app"
run_installer "$upgrade_dir"
[[ "$(bundle_version "$upgrade_dir/OpenCodeServer.app")" == "$candidate_version" ]]
upgrade_backups=("$upgrade_dir"/OpenCodeServer.app.backup.*(N))
(( ${#upgrade_backups} == 0 ))
assert_no_staging "$upgrade_dir"

requirements_dir="$(new_applications_dir incompatible-requirements)"
/usr/bin/ditto "$previous" "$requirements_dir/OpenCodeServer.app"
if run_installer "$requirements_dir" requirements; then
    print -u2 "simulated incompatible requirements unexpectedly installed"
    exit 1
fi
[[ "$(bundle_version "$requirements_dir/OpenCodeServer.app")" == "$previous_version" ]]
requirements_backups=("$requirements_dir"/OpenCodeServer.app.backup.*(N))
(( ${#requirements_backups} == 0 ))
assert_no_staging "$requirements_dir"

incompatible_dir="$(new_applications_dir actual-incompatible-requirements)"
baseline_app="$incompatible_dir/Baseline/OpenCodeServer.app"
incompatible_app="$incompatible_dir/Incompatible/OpenCodeServer.app"
/bin/mkdir -p "${baseline_app:h}"
/bin/mkdir -p "${incompatible_app:h}"
/usr/bin/ditto "$previous" "$baseline_app"
/usr/bin/ditto "$candidate" "$incompatible_app"
/usr/bin/codesign --force --sign - --options runtime \
    "$incompatible_app/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent"
/usr/bin/codesign --force --sign - --options runtime \
    --preserve-metadata=entitlements \
    "$incompatible_app/Contents/Resources/OpenCodeServerAgent.app"
/usr/bin/codesign --force --sign - --options runtime \
    "$incompatible_app/Contents/MacOS/opencodeserverctl"
/usr/bin/codesign --force --sign - --options runtime \
    --preserve-metadata=entitlements "$incompatible_app"
"$repo_root/scripts/validate_bundle.sh" "$incompatible_app"
if "$repo_root/scripts/check_requirements.sh" "$baseline_app" "$incompatible_app"; then
    print -u2 "incompatible Designated Requirement unexpectedly passed"
    exit 1
fi

for failure in staging-copy staging-validation; do
    failure_dir="$(new_applications_dir "$failure")"
    if run_installer "$failure_dir" "$failure"; then
        print -u2 "simulated $failure unexpectedly installed"
        exit 1
    fi
    [[ ! -e "$failure_dir/OpenCodeServer.app" ]]
    assert_no_staging "$failure_dir"
done

final_move_dir="$(new_applications_dir final-move)"
/usr/bin/ditto "$previous" "$final_move_dir/OpenCodeServer.app"
if run_installer "$final_move_dir" final-move; then
    print -u2 "simulated final move failure unexpectedly installed"
    exit 1
fi
[[ "$(bundle_version "$final_move_dir/OpenCodeServer.app")" == "$previous_version" ]]
final_move_backups=("$final_move_dir"/OpenCodeServer.app.backup.*(N))
(( ${#final_move_backups} == 0 ))
assert_no_staging "$final_move_dir"

# A final-validation failure must restore the previous bundle and leave no
# staging behind (the restore succeeded, so staging may be retired).
final_validation_dir="$(new_applications_dir final-validation)"
/usr/bin/ditto "$previous" "$final_validation_dir/OpenCodeServer.app"
if run_installer "$final_validation_dir" final-validation; then
    print -u2 "simulated final validation failure unexpectedly installed"
    exit 1
fi
[[ "$(bundle_version "$final_validation_dir/OpenCodeServer.app")" == "$previous_version" ]]
assert_no_staging "$final_validation_dir"

# A restore failure must NOT delete the staging directory: it holds the
# only copy of the previous bundle, and the error output must name the
# recovery path. The failed candidate is moved aside first, so the
# destination is empty.
restore_failure_dir="$(new_applications_dir restore-failure)"
/usr/bin/ditto "$previous" "$restore_failure_dir/OpenCodeServer.app"
if run_installer "$restore_failure_dir" restore-failure \
    2> "$test_root/restore-failure.log"
then
    print -u2 "simulated restore failure unexpectedly installed"
    exit 1
fi
restore_staging=("$restore_failure_dir"/.OpenCodeServer.install.*(N))
if (( ${#restore_staging} != 1 )); then
    print -u2 "restore failure did not preserve the staging directory"
    exit 1
fi
[[ -d "${restore_staging[1]}/PreviousOpenCodeServer.app" ]]
[[ -d "${restore_staging[1]}/RejectedOpenCodeServer.app" ]]
/usr/bin/grep -q "Manual recovery required" "$test_root/restore-failure.log"
/usr/bin/grep -qF "${restore_staging[1]}" "$test_root/restore-failure.log"
[[ ! -e "$restore_failure_dir/OpenCodeServer.app" ]]

# A signal arriving while the previous bundle is held in staging must
# restore the previous bundle and report the signal's exit code. The
# background installer's stdout/stderr is redirected to a log file so the
# command substitution collecting the PID is not blocked by the installer's
# inherited pipe descriptors. A watchdog guarantees the test never waits
# for the installer's 300-second pause.
signal_status=0
expected_status=0
for signal_spec in "HUP 129" "INT 130" "TERM 143"; do
    signal_name="${signal_spec%% *}"
    expected_status="${signal_spec##* }"
    signal_dir="$(new_applications_dir "signal-$signal_name")"
    /usr/bin/ditto "$previous" "$signal_dir/OpenCodeServer.app"
    run_installer_background "$signal_dir" pause-after-previous-held "$test_root/signal-$signal_name.log"
    previous_held=()
    attempts=0
    while (( attempts < 200 )); do
        previous_held=("$signal_dir"/.OpenCodeServer.install.*/PreviousOpenCodeServer.app(N))
        (( ${#previous_held} != 0 )) && break
        sleep 0.05
        (( attempts += 1 ))
    done
    if (( ${#previous_held} == 0 )); then
        print -u2 "installer never held the previous bundle ($signal_name)"
        exit 1
    fi
    /bin/kill "-$signal_name" "$installer_pid"
    ( sleep 10 && /bin/kill -KILL "$installer_pid" 2>/dev/null ) &
    watchdog=$!
    signal_status=0
    wait "$installer_pid" || signal_status=$?
    /bin/kill "$watchdog" 2>/dev/null
    wait "$watchdog" 2>/dev/null || true
    if (( signal_status == 137 )); then
        print -u2 "installer did not exit within 10 seconds of SIG$signal_name"
        exit 1
    fi
    if (( signal_status != expected_status )); then
        print -u2 "unexpected exit status $signal_status for SIG$signal_name (expected $expected_status)"
        exit 1
    fi
    if [[ "$(bundle_version "$signal_dir/OpenCodeServer.app")" != "$previous_version" ]]; then
        print -u2 "SIG$signal_name did not restore the previous bundle"
        exit 1
    fi
    assert_no_staging "$signal_dir"
done

# A signal arriving in the narrow window between the irreversible move of
# the previous bundle and the in-memory phase update must still restore the
# previous bundle. The filesystem-aware TRAPEXIT checks install_backup on
# disk, not the in-memory phase string.
boundary_dir="$(new_applications_dir signal-boundary)"
/usr/bin/ditto "$previous" "$boundary_dir/OpenCodeServer.app"
run_installer_background "$boundary_dir" pause-before-previous-held "$test_root/signal-boundary.log"
previous_held=()
attempts=0
while (( attempts < 200 )); do
    previous_held=("$boundary_dir"/.OpenCodeServer.install.*/PreviousOpenCodeServer.app(N))
    (( ${#previous_held} != 0 )) && break
    sleep 0.05
    (( attempts += 1 ))
done
if (( ${#previous_held} == 0 )); then
    print -u2 "installer never held the previous bundle (boundary)"
    exit 1
fi
/bin/kill -TERM "$installer_pid"
( sleep 10 && /bin/kill -KILL "$installer_pid" 2>/dev/null ) &
watchdog=$!
boundary_status=0
wait "$installer_pid" || boundary_status=$?
/bin/kill "$watchdog" 2>/dev/null
wait "$watchdog" 2>/dev/null || true
if (( boundary_status == 137 )); then
    print -u2 "installer did not exit within 10 seconds of the boundary signal"
    exit 1
fi
if (( boundary_status != 143 )); then
    print -u2 "unexpected boundary exit status $boundary_status (expected 143)"
    exit 1
fi
if [[ "$(bundle_version "$boundary_dir/OpenCodeServer.app")" != "$previous_version" ]]; then
    print -u2 "boundary signal did not restore the previous bundle"
    exit 1
fi
assert_no_staging "$boundary_dir"

# cleanup_staging must refuse to delete a staging directory whose identity
# changed (different inode), preserving it for manual inspection.
for cleanup_failure in staging-identity-changed staging-symlink; do
    cleanup_dir="$(new_applications_dir "$cleanup_failure")"
    if run_installer "$cleanup_dir" "$cleanup_failure" 2>/dev/null; then
        print -u2 "simulated $cleanup_failure unexpectedly installed"
        exit 1
    fi
    cleanup_staging=("$cleanup_dir"/.OpenCodeServer.install.*(N))
    if (( ${#cleanup_staging} != 1 )); then
        print -u2 "$cleanup_failure staging was unexpectedly cleaned"
        exit 1
    fi
    if [[ "$cleanup_failure" == "staging-symlink" && ! -L "${cleanup_staging[1]}" ]]; then
        print -u2 "staging-symlink did not preserve the symlink"
        exit 1
    fi
done

print "Install workflow tests passed"
