#!/usr/bin/env bash
# Build audia-tap from source and install its app bundle on macOS.
#
# This uses the system Swift compiler (Command Line Tools are enough; full
# Xcode is not required), assembles the .app from the project's Info.plist and
# entitlements, and ad-hoc signs it. No sudo is used.
set -euo pipefail

repo="${AUDIA_TAP_REPO:-https://github.com/Bibhav48/audia-tap}"
version="${AUDIA_TAP_VERSION:-v0.1.0}"
target="${AUDIA_TAP_TARGET:-$(uname -m)-apple-macosx26.2}"

if ! command -v swiftc >/dev/null 2>&1; then
    echo "error: swiftc not found; install the Xcode Command Line Tools" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "Cloning audia-tap $version..."
git clone --quiet "$repo" "$work/src"
git -C "$work/src" checkout --quiet "$version"

cd "$work/src"

echo "Building with swiftc for $target..."
swiftc -O -swift-version 5 -target "$target" \
    audia-tap/*.swift \
    -o "$work/audia-tap" \
    -framework AVFoundation \
    -framework AudioToolbox \
    -framework CoreGraphics \
    -framework OSLog

app="$work/audia-tap.app"
mkdir -p "$app/Contents/MacOS"
cp "$work/audia-tap" "$app/Contents/MacOS/audia-tap"

sed -e 's/\$(DEVELOPMENT_LANGUAGE)/en/g' \
    -e 's/\$(EXECUTABLE_NAME)/audia-tap/g' \
    -e 's/\$(PRODUCT_BUNDLE_IDENTIFIER)/com.audia.tap/g' \
    -e 's/\$(PRODUCT_NAME)/audia-tap/g' \
    audia-tap/Info.plist > "$app/Contents/Info.plist"

app_version="$(sed -n 's/.*audiaVersion = "\([^"]*\)".*/\1/p' audia-tap/main.swift | head -1)"
app_version="${app_version:-1.1.0}"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $app_version" "$app/Contents/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string 1" "$app/Contents/Info.plist" 2>/dev/null || true

codesign --force --sign - --entitlements audia-tap/audia-tap.entitlements "$app"

if [ -w /Applications ]; then
    destination="/Applications"
else
    destination="$HOME/Applications"
    mkdir -p "$destination"
fi

rm -rf "$destination/audia-tap.app"
cp -R "$app" "$destination/audia-tap.app"

mkdir -p "$HOME/.local/bin"
ln -sf "$destination/audia-tap.app/Contents/MacOS/audia-tap" "$HOME/.local/bin/audia-tap"

echo "Installed $destination/audia-tap.app"
echo "Linked $HOME/.local/bin/audia-tap"
echo
echo "Next: start the permission agent once and grant System Audio Recording:"
echo "  open -a \"$destination/audia-tap.app\" --args --agent"
echo "If macOS blocks the ad-hoc app, approve it in System Settings -> Privacy & Security."
