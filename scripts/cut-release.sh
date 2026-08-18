#!/bin/zsh
set -euo pipefail

# End-to-end public release:
#   1. scripts/package-release.sh — clean Release build with the configured
#      identity, ditto zip + dmg, zip round-trip signature verification;
#   2. create the GitHub Release with the versioned zip/dmg plus a stable
#      -latest.zip alias for curl installs;
#   3. notify the Homebrew tap (repository_dispatch) so its bump-cask
#      workflow updates version/sha256 automatically.
#
# Usage: scripts/cut-release.sh <release-notes.md>

repo_root="${0:A:h:h}"
cd "$repo_root"

if (( $# != 1 )) || [[ ! -r "$1" ]]; then
    print -u2 "usage: cut-release.sh <release-notes.md>"
    exit 2
fi
notes_path="${1:A}"

./scripts/package-release.sh

version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
    build/OpenCodeServer.app/Contents/Info.plist)"
short_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    build/OpenCodeServer.app/Contents/Info.plist)"

zip_path="build/dist/OpenCodeServer-${version}.zip"
dmg_path="build/dist/OpenCodeServer-${version}.dmg"
latest_path="build/dist/OpenCodeServer-latest.zip"
cp "$zip_path" "$latest_path"
(
    cd build/dist
    /usr/bin/shasum -a 256 "OpenCodeServer-${version}.zip" \
        >"OpenCodeServer-${version}.zip.sha256"
    /usr/bin/shasum -a 256 "OpenCodeServer-latest.zip" \
        >"OpenCodeServer-latest.zip.sha256"
)

gh release create "v${version}" \
    --repo Lavan1874/OpenCodeServer \
    --title "OpenCodeServer ${short_version} (Build ${version})" \
    --notes-file "$notes_path" \
    "$zip_path" "$zip_path.sha256" "$dmg_path" \
    "$latest_path" "$latest_path.sha256"

gh api repos/Lavan1874/homebrew-opencodeserver/dispatches \
    -f event_type=release-published

print "Released v${version}; the tap will bump its cask automatically."
