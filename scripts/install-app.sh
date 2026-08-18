#!/bin/bash
# OpenCodeServer one-line installer.
#
# Intended to be run as:
#
#   curl -fsSL https://raw.githubusercontent.com/Lavan1874/OpenCodeServer/main/scripts/install-app.sh | bash
#
# Downloads the latest prebuilt release over HTTPS from the project's
# GitHub Releases, verifies the published SHA-256 digest and the app's
# code signature, extracts with ditto (preserving the signature), and
# installs without the Gatekeeper quarantine prompt. No sudo: installs
# to /Applications when writable, otherwise ~/Applications. OpenCode
# itself must be installed separately.
#
# Internal/testing hooks: OPENCODESERVER_INSTALL_DIR overrides the
# destination directory; OPENCODESERVER_INSTALL_NO_OPEN=1 skips
# launching after install.

set -euo pipefail

base_url="https://github.com/Lavan1874/OpenCodeServer/releases/latest/download"
archive="OpenCodeServer-latest.zip"

case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) ;;
    Darwin/*)
        echo "OpenCodeServer requires Apple Silicon (arm64)." >&2
        exit 1
        ;;
    *)
        echo "OpenCodeServer is macOS-only." >&2
        exit 1
        ;;
esac

destination_dir="${OPENCODESERVER_INSTALL_DIR:-}"
custom_destination=0
if [[ -z "$destination_dir" ]]; then
    if [[ -w /Applications ]]; then
        destination_dir="/Applications"
    else
        destination_dir="$HOME/Applications"
        mkdir -p "$destination_dir"
    fi
else
    custom_destination=1
fi
destination="$destination_dir/OpenCodeServer.app"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
cd "$workdir"

echo "==> downloading the latest OpenCodeServer release"
curl -fsSL -o "$archive" "$base_url/$archive"
curl -fsSL -o "$archive.sha256" "$base_url/$archive.sha256"
shasum -a 256 -c "$archive.sha256"

echo "==> verifying the code signature"
ditto -x -k "$archive" .
codesign --verify --deep --strict OpenCodeServer.app

# A running copy must quit before its bundle is replaced; ask politely
# and wait briefly. Agent and OpenCode are intentionally untouched.
if (( custom_destination == 0 )) && [[ -d "$destination" ]] \
    && pgrep -x OpenCodeServer >/dev/null
then
    echo "==> asking the running OpenCodeServer to quit"
    osascript -e 'tell application id "ai.opencode.server" to quit' >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        pgrep -x OpenCodeServer >/dev/null || break
        sleep 1
    done
    if pgrep -x OpenCodeServer >/dev/null; then
        echo "OpenCodeServer is still running; quit it from the menu bar and re-run." >&2
        exit 1
    fi
fi

previous=""
if [[ -e "$destination" ]]; then
    previous="$destination.previous.$$"
    mv "$destination" "$previous"
fi
if ! ditto OpenCodeServer.app "$destination"; then
    if [[ -n "$previous" ]]; then
        mv "$previous" "$destination"
    fi
    echo "Unable to install to $destination." >&2
    exit 1
fi
rm -rf "$previous"

echo "==> installed: $destination"
if [[ "${OPENCODESERVER_INSTALL_NO_OPEN:-0}" != "1" ]]; then
    open "$destination"
    echo "OpenCodeServer is launching in the menu bar."
fi
