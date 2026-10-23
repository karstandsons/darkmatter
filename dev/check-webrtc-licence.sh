#!/usr/bin/env bash
# Refuses a libwebrtc (.webrtc/current, or the directory given) that has
# H.264, H.265 or FFmpeg in it, so no release links them (LGPL and patent
# licensing). Looks at the build's own arguments, the licences it gathered,
# and the library's symbols.
set -euo pipefail
dir=${1:-$(cd "$(dirname "$0")/.." && pwd)/.webrtc/current}
fail() { echo "libwebrtc in $dir: $*" >&2; exit 1; }
args=$dir/args.gn
[[ -f $args ]] || fail "no args.gn"
grep -Eq '^rtc_use_h264 *= *false' "$args" || fail "built with H.264"
grep -Eq '^rtc_use_h265 *= *false' "$args" || fail "built with H.265"
grep -Eq '^ffmpeg_branding *= *"Chrome"' "$args" && fail "built with Chrome's FFmpeg codecs"
if grep -Eiq '^# *(ffmpeg|openh264)$' "$dir/LICENSE.md"; then fail "has FFmpeg or OpenH264 among its licences"; fi
lib=$(find "$dir/lib" -name 'libwebrtc.a' | head -1)
[[ -n $lib ]] || fail "no libwebrtc.a"
nm=$(command -v llvm-nm || command -v nm)
if "$nm" --defined-only "$lib" 2>/dev/null | grep -Eiq 'avcodec_|ff_h264|WelsCreate'; then fail "has FFmpeg or OpenH264 code"; fi
echo "libwebrtc in $dir: no H.264, H.265 or FFmpeg"
