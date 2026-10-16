#!/usr/bin/env bash
# Build Darkmatter.app around a (universal) macOS binary and zip it:
#   packaging/macos/bundle.sh <binary> <iconset dir> [out dir (default dist)]
# Makes <out>/darkmatter-<version>-macos-universal.zip. Runs on macOS (iconutil,
# codesign, ditto). Signed ad hoc only: Gatekeeper still asks on first open.
set -euo pipefail
bin=$1 iconset=$2 out=${3:-dist}
# A full path, so a bare name ("darkmatter") isn't looked up in PATH.
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
here=$(cd "$(dirname "$0")" && pwd)
version=$("$bin" --version | awk '{print $2}')
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app=$stage/Darkmatter.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m755 "$bin" "$app/Contents/MacOS/darkmatter"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/darkmatter.icns"
cp "$here/../../LICENSE-MIT" "$here/../../LICENSE-APACHE" "$app/Contents/Resources/"
# The built-in plugins (they need python3; meme also ImageMagick).
cp -r "$here/../../plugins" "$app/Contents/Resources/plugins"
find "$app/Contents/Resources/plugins" \( -name __pycache__ -o -name .gitignore \) -prune -exec rm -rf {} +
# Apple Silicon refuses to run unsigned code; an ad-hoc signature is enough.
codesign --force --deep --sign - "$app"
mkdir -p "$out"
zip=$out/darkmatter-$version-macos-universal.zip
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
