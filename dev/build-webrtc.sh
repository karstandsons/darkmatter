#!/usr/bin/env bash
# Builds libwebrtc for calls without H.264 (OpenH264 and FFmpeg: LGPL and
# patent licensing) or, on Linux, H.265, and zips it as
# webrtc-<triple>-release.zip, laid out like LiveKit's prebuilt archives.
# dev/fetch-webrtc.sh fetches these from the release tagged
# webrtc-89d790b-free.
#
#   dev/build-webrtc.sh linux-x64|linux-arm64|mac-x64|mac-arm64 [work dir]
#
# It uses LiveKit's own build scripts and patches, at the rust-sdks commit
# that published webrtc-89d790b, with up to three arguments changed. macOS
# keeps H.265: there it's only the parser and Apple's hardware decoder
# (VideoToolbox, as its H.264 is), and its Objective-C codec factories don't
# build without it. Linux builds on Linux (arm64 is cross-built on x64),
# macOS on macOS. It needs about 20 GB and an hour or two on 8 cores; the
# work dir can be kept for rebuilds.
set -euo pipefail
triple=${1:?"usage: $0 linux-x64|linux-arm64|mac-x64|mac-arm64 [work dir]"}
work=${2:-$PWD/webrtc-build}
sdks_commit=24f7126929efde0da13d22e02d8c7a5a05be682d  # livekit/rust-sdks
webrtc_commit=89d790b40447c3c5c54c3edd58aa53d285e35fa7  # webrtc-sdk/webrtc m150
os=${triple%-*} arch=${triple#*-}
case $os in linux) script=build_linux.sh ;; mac) script=build_macos.sh ;; *) echo "unknown triple $triple" >&2; exit 1 ;; esac
here=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$work"
if [[ ! -d $work/rust-sdks ]]; then
  git clone --filter=blob:none https://github.com/livekit/rust-sdks.git "$work/rust-sdks"
fi
git -C "$work/rust-sdks" checkout -q "$sdks_commit"
cd "$work/rust-sdks/webrtc-sys/libwebrtc"
# The exact WebRTC commit, not the branch it was on.
sed -i.orig "s|webrtc.git@[^']*'|webrtc.git@$webrtc_commit'|" .gclient
# The free build: no H.264 (which brings OpenH264 and FFmpeg), no FFmpeg
# proprietary codecs, no H.265 on Linux. And patches applied only once, so a
# kept work dir can build again.
h265=true
[[ $os == linux ]] && h265=false
sed -e 's/ffmpeg_branding=\\"Chrome\\"/ffmpeg_branding=\\"Chromium\\"/' \
    -e 's/rtc_use_h264=true/rtc_use_h264=false/' \
    -e "s/rtc_use_h265=true/rtc_use_h265=$h265/" \
    -E -e 's#^(git (-C build )?)apply ("\$COMMAND_DIR/patches/[^"]+") (-v .*)$#\1apply --reverse --check \3 --ignore-space-change --ignore-whitespace 2>/dev/null || \1apply \3 \4#' \
    "$script" > "free-$script"
grep -q 'rtc_use_h264=false' "free-$script" && grep -q "rtc_use_h265=$h265" "free-$script" || { echo "couldn't set H.264/H.265 in $script" >&2; exit 1; }
chmod +x "free-$script"
"./free-$script" --arch "$arch"

out=$PWD/$triple-release
"$here/check-webrtc-licence.sh" "$out"
zip=$work/webrtc-$triple-release.zip
rm -f "$zip"
zip -qr -X "$zip" "$triple-release"
if command -v sha256sum >/dev/null; then sha256sum "$zip"; else shasum -a 256 "$zip"; fi
