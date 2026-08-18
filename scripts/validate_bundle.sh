#!/bin/zsh
set -euo pipefail

if (( $# != 1 )); then
    print -u2 "usage: validate_bundle.sh <OpenCodeServer.app>"
    exit 2
fi

app_path="${1:A}"
if [[ "$app_path" != */OpenCodeServer.app ]]; then
    print -u2 "unexpected app bundle name"
    exit 2
fi

lipo_tool="/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/lipo"
if [[ ! -x "$lipo_tool" ]]; then
    lipo_tool="/Library/Developer/CommandLineTools/usr/bin/lipo"
fi

required_files=(
    Contents/Info.plist
    Contents/PkgInfo
    Contents/MacOS/OpenCodeServer
    Contents/Resources/OpenCodeServerAgent.app/Contents/Info.plist
    Contents/Resources/OpenCodeServerAgent.app/Contents/PkgInfo
    Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent
    Contents/MacOS/opencodeserverctl
    Contents/Library/LaunchAgents/ai.opencode.server.agent.plist
    Contents/Resources/AppIcon.icns
)

for relative_path in "${required_files[@]}"; do
    if [[ ! -e "$app_path/$relative_path" ]]; then
        print -u2 "missing bundle item: $relative_path"
        exit 1
    fi
done

/usr/bin/plutil -lint \
    "$app_path/Contents/Info.plist" \
    "$app_path/Contents/Library/LaunchAgents/ai.opencode.server.agent.plist"

for executable_path in \
    "$app_path/Contents/MacOS/OpenCodeServer" \
    "$app_path/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent" \
    "$app_path/Contents/MacOS/opencodeserverctl"
do
    architectures="$("$lipo_tool" -archs "$executable_path")"
    if [[ "$architectures" != "arm64" ]]; then
        print -u2 "\${executable_path:t} has unexpected architectures: $architectures"
        exit 1
    fi
done

# TN3179: Local Network privacy uses the main executable UUID as part of its
# identity tracking. Keep this structural prerequisite machine-checkable for
# every native binary inside the product bundle. The external OpenCode child
# is checked separately during installed acceptance because it is selected by
# the user and is not part of this signed bundle.
if ! command -v dwarfdump >/dev/null 2>&1; then
    print -u2 "dwarfdump is required to validate LC_UUID"
    exit 1
fi
typeset -A seen_uuids
for executable_path in \
    "$app_path/Contents/MacOS/OpenCodeServer" \
    "$app_path/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent" \
    "$app_path/Contents/MacOS/opencodeserverctl"
do
    uuid="$(dwarfdump --uuid "$executable_path" | awk '/^UUID:/ { print $2; exit }')"
    if [[ -z "$uuid" ]]; then
        print -u2 "missing LC_UUID: $executable_path"
        exit 1
    fi
    if [[ -n "${seen_uuids[$uuid]:-}" ]]; then
        print -u2 "duplicate LC_UUID $uuid: ${seen_uuids[$uuid]} and $executable_path"
        exit 1
    fi
    seen_uuids[$uuid]="$executable_path"
done

bundle_identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_path/Contents/Info.plist")"
[[ "$bundle_identifier" == "ai.opencode.server" ]]

lsui_element="$(/usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$app_path/Contents/Info.plist")"
[[ "$lsui_element" == "true" ]]

main_storyboard="$(/usr/libexec/PlistBuddy -c 'Print :NSMainStoryboardFile' "$app_path/Contents/Info.plist")"
[[ "$main_storyboard" == "Main" ]]

local_network_purpose="$(/usr/libexec/PlistBuddy -c 'Print :NSLocalNetworkUsageDescription' "$app_path/Contents/Info.plist")"
expected_local_network_purpose="OpenCodeServer advertises your configured OpenCode service on the local network when you enable mDNS."
if [[ "$local_network_purpose" != "$expected_local_network_purpose" ]]; then
    print -u2 "unexpected Local Network purpose string: $local_network_purpose"
    exit 1
fi

bundle_program="$(/usr/libexec/PlistBuddy -c 'Print :BundleProgram' "$app_path/Contents/Library/LaunchAgents/ai.opencode.server.agent.plist")"
[[ "$bundle_program" == "Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent" ]]

associated_bundle="$(/usr/libexec/PlistBuddy -c 'Print :AssociatedBundleIdentifiers:0' "$app_path/Contents/Library/LaunchAgents/ai.opencode.server.agent.plist")"
if [[ "$associated_bundle" != "ai.opencode.server" ]]; then
    print -u2 "unexpected AssociatedBundleIdentifiers entry: $associated_bundle"
    exit 1
fi

# Keep the launch agent in an app-like bundle. TN3179 and Apple's DTS
# guidance require a real bundle identity when a launchd agent performs
# Bonjour work; embedding a plist in the bare Mach-O was insufficient on
# macOS 26.
agent_bundle="$app_path/Contents/Resources/OpenCodeServerAgent.app"
agent_executable="$agent_bundle/Contents/MacOS/OpenCodeServerAgent"
/usr/bin/plutil -lint "$agent_bundle/Contents/Info.plist"
agent_display_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleDisplayName' "$agent_bundle/Contents/Info.plist")"
agent_identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$agent_bundle/Contents/Info.plist")"
agent_executable_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$agent_bundle/Contents/Info.plist")"
agent_package_type="$(/usr/libexec/PlistBuddy -c 'Print :CFBundlePackageType' "$agent_bundle/Contents/Info.plist")"
agent_local_network_purpose="$(/usr/libexec/PlistBuddy -c 'Print :NSLocalNetworkUsageDescription' "$agent_bundle/Contents/Info.plist")"
[[ "$agent_display_name" == "OpenCodeServer" ]]
[[ "$agent_identifier" == "ai.opencode.server.agent" ]]
[[ "$agent_executable_name" == "OpenCodeServerAgent" ]]
[[ "$agent_package_type" == "APPL" ]]
[[ "$agent_local_network_purpose" == "$expected_local_network_purpose" ]]

# ADR 0012: the agent must stay Interactive; Background throttling stalls the
# SMAppService launch trampoline for tens of seconds during the login storm.
process_type="$(/usr/libexec/PlistBuddy -c 'Print :ProcessType' "$app_path/Contents/Library/LaunchAgents/ai.opencode.server.agent.plist")"
if [[ "$process_type" != "Interactive" ]]; then
    print -u2 "agent ProcessType must be Interactive, got: $process_type"
    exit 1
fi

/usr/bin/codesign --verify --deep --strict --verbose=2 \
    "$agent_bundle"
/usr/bin/codesign --verify --strict --verbose=2 \
    "$app_path/Contents/MacOS/opencodeserverctl"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$app_path"

for signed_item in \
    "$app_path" \
    "$agent_bundle" \
    "$agent_executable" \
    "$app_path/Contents/MacOS/opencodeserverctl"
do
    signature_details="$(/usr/bin/codesign --display --verbose=4 "$signed_item" 2>&1)"
    if [[ "$signature_details" != *runtime* ]]; then
        print -u2 "hardened runtime flag is missing: $signed_item"
        exit 1
    fi
    entitlements="$(/usr/bin/codesign --display --entitlements - "$signed_item" 2>/dev/null || true)"
    if [[ "$entitlements" == *com.apple.security.app-sandbox* ]]; then
        print -u2 "App Sandbox must remain disabled: $signed_item"
        exit 1
    fi
done

print "Bundle validation passed: $app_path"
