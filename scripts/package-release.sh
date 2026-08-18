#!/bin/zsh
set -euo pipefail

# Build the distributable Release bundle and package it as a ditto zip and
# a dmg, both preserving the code signature. Verifies that the zip
# round-trips through `ditto -x -k` with a strict signature check before
# the artifacts are considered publishable, and prints their sha256
# digests for the GitHub Release and the Homebrew cask.
#
# The Release build itself goes through scripts/build.sh with the
# configured Apple Development identity
# (~/.config/opencodeserver/signing-identity or $SIGNING_IDENTITY).

repo_root="${0:A:h:h}"
cd "$repo_root"

CLEAN_BUILD="${CLEAN_BUILD:-1}" ./scripts/build.sh

version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
    build/OpenCodeServer.app/Contents/Info.plist)"

dist_dir="$repo_root/build/dist"
mkdir -p "$dist_dir"
zip_path="$dist_dir/OpenCodeServer-${version}.zip"
dmg_path="$dist_dir/OpenCodeServer-${version}.dmg"
rm -f "$zip_path" "$dmg_path"

/usr/bin/ditto -c -k --keepParent build/OpenCodeServer.app "$zip_path"
/usr/bin/hdiutil create \
    -volname OpenCodeServer \
    -srcfolder build/OpenCodeServer.app \
    -ov -format UDZO \
    "$dmg_path" >/dev/null

verify_dir="$(mktemp -d)"
trap 'rm -rf "$verify_dir"' EXIT
/usr/bin/ditto -x -k "$zip_path" "$verify_dir"
/usr/bin/codesign --verify --deep --strict "$verify_dir/OpenCodeServer.app"

print "version: $version"
print "zip:     $zip_path"
print "dmg:     $dmg_path"
/usr/bin/shasum -a 256 "$zip_path" "$dmg_path"
