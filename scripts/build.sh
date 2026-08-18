#!/bin/zsh
set -euo pipefail

repo_root="${0:A:h:h}"
cd "$repo_root"

if [[ "$(/usr/bin/uname -m)" != "arm64" ]]; then
    print -u2 "OpenCodeServer v1 builds only on Apple Silicon"
    exit 1
fi

xcode_developer="/Applications/Xcode.app/Contents/Developer"
if [[ ! -x "$xcode_developer/usr/bin/xcodebuild" ]]; then
    print -u2 "Xcode 26 with the macOS 26 SDK is required"
    exit 1
fi

export DEVELOPER_DIR="$xcode_developer"
configuration="${CONFIGURATION:-Release}"
if [[ "$configuration" != "Debug" && "$configuration" != "Release" ]]; then
    print -u2 "CONFIGURATION must be Debug or Release"
    exit 2
fi

derived_data="$repo_root/build/XcodeDerivedData"
local_identity_file="$HOME/.config/opencodeserver/signing-identity"
if [[ -n "${SIGNING_IDENTITY:-}" ]]; then
    identity="$SIGNING_IDENTITY"
elif [[ "$configuration" == "Release" && -r "$local_identity_file" ]]; then
    identity="$(
        /usr/bin/sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' "$local_identity_file"
    )"
    if [[ -z "$identity" || "$identity" == *$'\n'* ]]; then
        print -u2 "$local_identity_file must contain exactly one identity name"
        exit 2
    fi
elif [[ "$configuration" == "Release" ]]; then
    print -u2 "Release builds need a signing identity: set SIGNING_IDENTITY or put the"
    print -u2 "identity name in $local_identity_file (see docs/signing-identity.example.md)"
    exit 2
else
    identity="-"
fi
xcode_arguments=(
    -project "$repo_root/OpenCodeServer.xcodeproj"
    -scheme OpenCodeServer
    -configuration "$configuration"
    -derivedDataPath "$derived_data"
    -destination "platform=macOS,arch=arm64"
    CODE_SIGN_STYLE=Manual
    CODE_SIGN_IDENTITY="$identity"
)

# Xcode intentionally omits the runtime option for an ad hoc identity. Force it
# only for local structural verification; stable TCC testing requires a real
# signing identity.
if [[ "$identity" == "-" ]]; then
    xcode_arguments+=("OTHER_CODE_SIGN_FLAGS=--options runtime")
fi

if [[ "${CLEAN_BUILD:-0}" == "1" ]]; then
    "$xcode_developer/usr/bin/xcodebuild" "${xcode_arguments[@]}" clean
fi
"$xcode_developer/usr/bin/xcodebuild" "${xcode_arguments[@]}" build

app_path="$derived_data/Build/Products/$configuration/OpenCodeServer.app"
if [[ ! -d "$app_path" ]]; then
    print -u2 "Xcode did not produce the expected application: $app_path"
    exit 1
fi
"$repo_root/scripts/validate_bundle.sh" "$app_path"

final_app="$repo_root/build/OpenCodeServer.app"
/bin/rm -rf "$final_app"
/usr/bin/ditto "$app_path" "$final_app"
"$repo_root/scripts/validate_bundle.sh" "$final_app"
print "Built $final_app"
