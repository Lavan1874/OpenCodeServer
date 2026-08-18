#!/bin/zsh
set -euo pipefail

repo_root="${0:A:h:h}"
default_applications_dir="/Applications"
local_identity_file="$HOME/.config/opencodeserver/signing-identity"
if [[ -n "${OPENCODESERVER_SIGNING_AUTHORITY:-}" ]]; then
    required_signing_authority="$OPENCODESERVER_SIGNING_AUTHORITY"
elif [[ -r "$local_identity_file" ]]; then
    required_signing_authority="$(
        /usr/bin/sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' "$local_identity_file"
    )"
    if [[ -z "$required_signing_authority" \
        || "$required_signing_authority" == *$'\n'* ]]
    then
        print -u2 "$local_identity_file must contain exactly one identity name"
        exit 2
    fi
else
    print -u2 "The installer needs the required signing authority: set"
    print -u2 "OPENCODESERVER_SIGNING_AUTHORITY or put the identity name in"
    print -u2 "$local_identity_file (see docs/signing-identity.example.md)"
    exit 2
fi

typeset -g install_applications_dir=""
typeset -g install_staging_root=""
typeset -g install_staging_identity=""
typeset -g install_destination=""
typeset -g install_backup=""
typeset -g install_testing=0
typeset -g install_test_failure=""
# Transaction phase: setup → staged → previous-held → candidate-installed →
# verified. TRAPEXIT uses it to decide between cleaning the staging
# directory (the previous bundle is not inside it yet) and restoring the
# previous bundle first. Once the previous bundle is held in staging, the
# staging directory is NEVER deleted before the restore succeeded.
typeset -g install_phase="setup"
typeset -g install_pause_pid=""

bundle_version() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$1/Contents/Info.plist"
}

validate_stable_signature() {
    local app_path="$1"
    local signed_item
    local authority
    for signed_item in \
        "$app_path" \
        "$app_path/Contents/Resources/OpenCodeServerAgent.app" \
        "$app_path/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent" \
        "$app_path/Contents/MacOS/opencodeserverctl"
    do
        authority="$(
            /usr/bin/codesign --display --verbose=4 "$signed_item" 2>&1 \
                | /usr/bin/sed -n 's/^Authority=//p' \
                | /usr/bin/sed -n '1p'
        )"
        if [[ "$authority" != "$required_signing_authority" ]]; then
            print -u2 "unexpected signing authority: $signed_item"
            return 1
        fi
    done
}

version_is_strictly_newer() {
    local candidate="$1"
    local previous="$2"
    if [[ ! "$candidate" =~ '^[0-9]+([.][0-9]+){0,2}$' \
        || ! "$previous" =~ '^[0-9]+([.][0-9]+){0,2}$' ]]
    then
        return 2
    fi

    /usr/bin/awk -v candidate="$candidate" -v previous="$previous" '
        BEGIN {
            split(candidate, candidate_parts, ".")
            split(previous, previous_parts, ".")
            for (part_index = 1; part_index <= 3; part_index++) {
                candidate_value = candidate_parts[part_index] + 0
                previous_value = previous_parts[part_index] + 0
                if (candidate_value > previous_value) {
                    exit 0
                }
                if (candidate_value < previous_value) {
                    exit 1
                }
            }
            exit 1
        }
    '
}

validate_test_applications_dir() {
    local applications_dir="$1"
    if [[ "$applications_dir" != /private/tmp/OpenCodeServer-install-tests.*/* \
        || ! -d "$applications_dir" \
        || -L "$applications_dir" ]]
    then
        print -u2 "unsafe test applications directory: $applications_dir"
        return 1
    fi
}

cleanup_staging() {
    local current_identity
    if [[ -z "$install_staging_root" || ! -e "$install_staging_root" ]]; then
        return
    fi
    if [[ -z "$install_applications_dir" \
        || "${install_staging_root:h}" != "$install_applications_dir" \
        || "${install_staging_root:t}" != .OpenCodeServer.install.* \
        || -L "$install_staging_root" \
        || ! -d "$install_staging_root" ]]
    then
        print -u2 "Refusing to clean an unrecognized staging path: $install_staging_root"
        return
    fi
    current_identity="$(/usr/bin/stat -f '%u:%d:%i' "$install_staging_root")" || {
        print -u2 "Unable to verify staging directory before cleanup: $install_staging_root"
        return
    }
    if [[ -z "$install_staging_identity" \
        || "$current_identity" != "$install_staging_identity" ]]
    then
        print -u2 "Refusing to clean a staging directory whose identity changed: $install_staging_root"
        return
    fi
    /bin/rm -R "$install_staging_root"
    install_staging_root=""
    install_staging_identity=""
}

restore_previous_bundle() {
    local rejected_bundle
    if [[ -e "$install_destination" ]]; then
        rejected_bundle="$install_staging_root/RejectedOpenCodeServer.app"
        if [[ -e "$rejected_bundle" ]] \
            || ! /bin/mv "$install_destination" "$rejected_bundle"
        then
            print -u2 "Unable to move the failed candidate aside; previous bundle remains at $install_backup"
            return 1
        fi
    fi
    if [[ -z "$install_backup" || ! -d "$install_backup" ]]; then
        return 0
    fi
    if [[ "$install_test_failure" == "restore-failure" ]]; then
        print -u2 "simulated restore failure; previous bundle remains at $install_backup"
        return 1
    fi
    if /bin/mv "$install_backup" "$install_destination"; then
        print -u2 "Restored previous bundle at $install_destination"
        install_backup=""
        return 0
    fi
    print -u2 "Unable to restore previous bundle; it remains at $install_backup"
    return 1
}

install_main() {
    local identity_change=0
    local expected_previous_authority=""
    local expected_candidate_authority=""
    local source_argument=""
    if (( $# == 1 )); then
        source_argument="$1"
    elif (( $# == 4 )) && [[ "$1" == "--identity-change" ]]; then
        identity_change=1
        expected_previous_authority="$2"
        expected_candidate_authority="$3"
        source_argument="$4"
    else
        print -u2 "usage: install.sh [--identity-change <previous-authority> <candidate-authority>] <built OpenCodeServer.app>"
        return 2
    fi

    install_testing="${OPENCODESERVER_INSTALL_TESTING:-0}"
    install_test_failure="${OPENCODESERVER_INSTALL_TEST_FAILURE:-}"
    if [[ "$install_testing" == "1" ]]; then
        local requested_applications_dir="${OPENCODESERVER_INSTALL_APPLICATIONS_DIR:-}"
        if [[ -z "$requested_applications_dir" ]]; then
            print -u2 "test applications directory is required"
            return 2
        fi
        install_applications_dir="${requested_applications_dir:A}"
        validate_test_applications_dir "$install_applications_dir" || return $?
    else
        if [[ -n "${OPENCODESERVER_INSTALL_APPLICATIONS_DIR:-}" \
            || -n "$install_test_failure" ]]
        then
            print -u2 "installation test controls require OPENCODESERVER_INSTALL_TESTING=1"
            return 2
        fi
        install_applications_dir="$default_applications_dir"
    fi
    if [[ ! -d "$install_applications_dir" || -L "$install_applications_dir" ]]; then
        print -u2 "applications directory must be an existing non-symlink directory"
        return 1
    fi

    local source_app="${source_argument:A}"
    install_destination="$install_applications_dir/OpenCodeServer.app"
    if [[ "$source_app" != */OpenCodeServer.app \
        || "$source_app" == "$install_destination" ]]
    then
        print -u2 "source must be a separate OpenCodeServer.app bundle"
        return 2
    fi

    "$repo_root/scripts/validate_bundle.sh" "$source_app" || return $?
    validate_stable_signature "$source_app" || return $?
    local source_version
    source_version="$(bundle_version "$source_app")" || return $?

    if [[ "$install_applications_dir" == "$default_applications_dir" ]] \
        && /usr/bin/pgrep -x OpenCodeServer >/dev/null
    then
        print -u2 "Quit the installed OpenCodeServer before replacing its bundle. OpenCodeServerAgent and OpenCode may remain running."
        return 1
    fi

    if [[ -d "$install_destination" ]]; then
        local installed_version
        installed_version="$(bundle_version "$install_destination")" || return $?
        if ! version_is_strictly_newer "$source_version" "$installed_version"; then
            print -u2 "candidate CFBundleVersion must be greater than the installed version"
            return 1
        fi
    elif [[ -e "$install_destination" ]]; then
        print -u2 "installation destination exists but is not an app directory"
        return 1
    fi

    install_staging_root="$(
        /usr/bin/mktemp -d "$install_applications_dir/.OpenCodeServer.install.XXXXXXXX"
    )" || return $?
    install_staging_identity="$(
        /usr/bin/stat -f '%u:%d:%i' "$install_staging_root"
    )" || return $?
    local staged_app="$install_staging_root/OpenCodeServer.app"

    if [[ "$install_test_failure" == "staging-identity-changed" ]]; then
        /bin/rm -Rf "$install_staging_root"
        /bin/mkdir "$install_staging_root"
        print -u2 "simulated staging identity change"
        return 1
    fi
    if [[ "$install_test_failure" == "staging-symlink" ]]; then
        local symlink_target="$install_applications_dir/.ocs-symlink-target"
        /bin/mv "$install_staging_root" "$symlink_target"
        /bin/ln -s "$symlink_target" "$install_staging_root"
        print -u2 "simulated staging replaced with symlink"
        return 1
    fi

    if [[ "$install_test_failure" == "staging-copy" ]]; then
        print -u2 "simulated staging copy failure"
        return 1
    fi
    /usr/bin/ditto "$source_app" "$staged_app" || return $?

    if [[ "$install_test_failure" == "staging-validation" ]]; then
        print -u2 "simulated staging validation failure"
        return 1
    fi
    "$repo_root/scripts/validate_bundle.sh" "$staged_app" || return $?
    validate_stable_signature "$staged_app" || return $?
    # The staged candidate is valid; the destination is still untouched, so
    # from here any failure may simply clean the staging directory until the
    # previous bundle is moved into it.
    install_phase="staged"

    if [[ -d "$install_destination" ]]; then
        if [[ "$install_test_failure" == "requirements" ]]; then
            print -u2 "simulated Designated Requirement incompatibility"
            return 1
        fi
        if (( identity_change == 1 )); then
            "$repo_root/scripts/check_requirements.sh" \
                --identity-change \
                "$install_destination" "$staged_app" \
                "$expected_previous_authority" "$expected_candidate_authority" \
                || return $?
        else
            "$repo_root/scripts/check_requirements.sh" \
                "$install_destination" "$staged_app" || return $?
        fi
        # The previous bundle lives inside the staging directory for the
        # rest of the transaction: restored in place on failure, removed
        # with the staging directory on success. No same-bundle-identifier
        # copy is left behind in the applications directory (see the ADR
        # 0006 addendum on BTM resolution poisoning). While it is held here
        # the staging directory must survive every failure path until the
        # restore has succeeded.
        install_backup="$install_staging_root/PreviousOpenCodeServer.app"
        /bin/mv "$install_destination" "$install_backup" || return $?

        if [[ "$install_test_failure" == "pause-before-previous-held" ]]; then
            print -u2 "pausing after moving the previous bundle, before phase update"
            /bin/sleep 300 &
            install_pause_pid=$!
            wait "$install_pause_pid" || true
            install_pause_pid=""
        fi
        install_phase="previous-held"

        if [[ "$install_test_failure" == "pause-after-previous-held" ]]; then
            print -u2 "pausing with the previous bundle held in staging"
            /bin/sleep 300 &
            install_pause_pid=$!
            wait "$install_pause_pid" || true
            install_pause_pid=""
        fi
    fi

    if [[ "$install_test_failure" == "final-move" ]] \
        || ! /bin/mv "$staged_app" "$install_destination"
    then
        print -u2 "Unable to move the candidate into its final location"
        return 1
    fi
    install_phase="candidate-installed"

    if [[ "$install_test_failure" == "restore-failure" ]]; then
        print -u2 "simulated post-install failure; restore will be attempted"
        return 1
    fi

    if [[ "$install_test_failure" == "final-validation" ]]; then
        print -u2 "simulated final installed bundle verification failure"
        return 1
    fi

    if ! "$repo_root/scripts/validate_bundle.sh" "$install_destination" \
        || ! validate_stable_signature "$install_destination" \
        || [[ "$(bundle_version "$install_destination")" != "$source_version" ]]
    then
        print -u2 "Final installed bundle verification failed"
        return 1
    fi

    # The installed bundle is verified. The previous bundle's only remaining
    # copy lives in staging; the product decision is that a successful
    # install keeps no backup, so the staging cleanup below retires it.
    install_phase="verified"
    print "Installed $install_destination (CFBundleVersion $source_version)"
    print "Open the app once to register or refresh its background components."
}

# Single transaction epilogue for every exit path (normal return, error, and
# signal traps, which only set the exit code). Restore is attempted at most
# once, here; a failed restore preserves the staging directory and reports
# its exact path so the previous bundle is never silently destroyed.
TRAPEXIT() {
    if [[ -n "$install_pause_pid" ]]; then
        /bin/kill "$install_pause_pid" 2>/dev/null
        install_pause_pid=""
    fi
    # The in-memory phase can lag behind an irreversible move if a signal
    # arrives between the move and the next assignment. The previous bundle
    # on disk is the authoritative fact: if it exists in staging, restore is
    # required unless the install was fully verified.
    if [[ -n "$install_backup" && -d "$install_backup" ]]; then
        if [[ "$install_phase" == "verified" ]]; then
            cleanup_staging
        else
            restore_previous_bundle || print -u2 \
                "Manual recovery required: the previous bundle is preserved at $install_backup"
            if [[ -n "$install_backup" ]]; then
                return
            fi
            cleanup_staging
        fi
    else
        cleanup_staging
    fi
}

trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

typeset -i install_status=0
install_main "$@" || install_status=$?
exit "$install_status"
