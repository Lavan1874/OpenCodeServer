#!/bin/zsh
set -euo pipefail

repo_root="${PROJECT_DIR:A}"
if [[ "$(/usr/bin/uname -m)" != "arm64" ]]; then
    print -u2 "OpenCodeServer v1 Rust components must be built on Apple Silicon"
    exit 1
fi

cargo_bin="${CARGO_BIN:-/opt/homebrew/bin/cargo}"
if [[ ! -x "$cargo_bin" ]]; then
    cargo_bin="$(command -v cargo || true)"
fi
if [[ -z "$cargo_bin" || ! -x "$cargo_bin" ]]; then
    print -u2 "cargo was not found; install the stable Rust toolchain"
    exit 1
fi

case "$CONFIGURATION" in
    Debug)
        cargo_profile="debug"
        cargo_configuration=()
        ;;
    Release)
        cargo_profile="release"
        cargo_configuration=(--release)
        ;;
    *)
        print -u2 "unsupported Xcode configuration: $CONFIGURATION"
        exit 1
        ;;
esac

cargo_features=()
if [[ "${OPENCODESERVER_DIAGNOSTIC_LOCAL_NETWORK:-0}" == "1" ]]; then
    cargo_features+=(--features diagnostic-local-network)
fi

rust_target_dir="$DERIVED_FILE_DIR/RustTarget"
export CARGO_TARGET_DIR="$rust_target_dir"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:?Xcode must pass MACOSX_DEPLOYMENT_TARGET (see Config/Base.xcconfig)}"

# Bake the parent bundle's build identity into the OpenCodeServerAgent
# binary (see build.rs). OpenCodeServer requires this exact value before it
# commits a Service Management registration transaction, so a missing value
# must fail the Xcode build rather than ship an unverifiable agent.
if [[ -z "${CURRENT_PROJECT_VERSION:-}" ]]; then
    print -u2 "CURRENT_PROJECT_VERSION is not set; the agent build identity would be unverifiable"
    exit 1
fi
export OPENCODESERVER_BUNDLE_VERSION="$CURRENT_PROJECT_VERSION"

cd "$repo_root"
"$cargo_bin" build --locked "${cargo_configuration[@]}" "${cargo_features[@]}" \
    --bin OpenCodeServerAgent \
    --bin opencodeserverctl

agent_source="$rust_target_dir/$cargo_profile/OpenCodeServerAgent"
cli_source="$rust_target_dir/$cargo_profile/opencodeserverctl"
destination="$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH"
app_contents="${destination:h}"
agent_bundle="$app_contents/Resources/OpenCodeServerAgent.app"
agent_destination="$agent_bundle/Contents/MacOS/OpenCodeServerAgent"
cli_destination="$destination/opencodeserverctl"

# The launch agent is an app-like bundle so macOS can resolve its user-visible
# identity when the child OpenCode performs Bonjour operations. Keep the
# helper in the signed app's Resources directory and never leave a second,
# unsigned standalone copy at Contents/MacOS.
/bin/rm -Rf "$agent_bundle"
/bin/mkdir -p "$agent_destination:h"
/usr/bin/install -m 0644 \
    "$repo_root/resources/OpenCodeServerAgent-Info.plist" \
    "$agent_bundle/Contents/Info.plist"
/usr/bin/install -m 0644 \
    "$repo_root/resources/OpenCodeServerAgent-PkgInfo" \
    "$agent_bundle/Contents/PkgInfo"

for executable in "$agent_source" "$cli_source"; do
    if [[ ! -x "$executable" ]]; then
        print -u2 "Cargo did not produce expected executable: $executable"
        exit 1
    fi
done

/usr/bin/install -m 0755 "$agent_source" "$agent_destination"
/usr/bin/install -m 0755 "$cli_source" "$cli_destination"

lipo_tool="$DEVELOPER_DIR/Toolchains/XcodeDefault.xctoolchain/usr/bin/lipo"
for executable in "$agent_destination" "$cli_destination"; do
    architectures="$("$lipo_tool" -archs "$executable")"
    if [[ "$architectures" != "arm64" ]]; then
        print -u2 "${executable:t} has unexpected architectures: $architectures"
        exit 1
    fi
done

if [[ "${CODE_SIGNING_ALLOWED:-YES}" == "YES" ]]; then
    identity="${EXPANDED_CODE_SIGN_IDENTITY:-${CODE_SIGN_IDENTITY:--}}"
    if [[ -z "$identity" ]]; then
        identity="-"
    fi

    if [[ "$identity" != "-"
        && "${OPENCODESERVER_CODE_SIGN_TIMESTAMP:-0}" == "1"
    ]]; then
        timestamp_options=(--timestamp)
    else
        timestamp_options=(--timestamp=none)
    fi

    /usr/bin/codesign --force --options runtime "${timestamp_options[@]}" \
        --generate-entitlement-der \
        --sign "$identity" \
        --identifier ai.opencode.server.agent \
        --entitlements "$repo_root/resources/OpenCodeServerAgent.entitlements" \
        "$agent_bundle"

    /usr/bin/codesign --force --options runtime "${timestamp_options[@]}" \
        --generate-entitlement-der \
        --sign "$identity" \
        --identifier ai.opencode.server.ctl \
        "$cli_destination"

    /usr/bin/codesign --verify --deep --strict --verbose=2 "$agent_bundle"
    /usr/bin/codesign --verify --strict --verbose=2 "$cli_destination"
fi
