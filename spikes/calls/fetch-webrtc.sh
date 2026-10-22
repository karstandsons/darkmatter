#!/usr/bin/env bash
# Downloads LiveKit's prebuilt libwebrtc and checks it against the SHA-256
# GitHub published for the release asset, instead of letting webrtc-sys's
# build script fetch it unchecked. .cargo/config.toml points the build at it.
set -euo pipefail
tag=webrtc-89d790b  # webrtc-sys-build 0.3.20's WEBRTC_TAG
asset=webrtc-linux-x64-release
sha=b167adad5291cea0e4d66a0454d9d52d2ad714e6b0ed70f4410317d3ebde70c5
dir=${1:-$(dirname "$0")/.webrtc}
if [[ ! -d $dir/linux-x64-release ]]; then
  mkdir -p "$dir"
  curl -sSfL -o "$dir/$asset.zip" "https://github.com/livekit/rust-sdks/releases/download/$tag/$asset.zip"
  echo "$sha  $dir/$asset.zip" | sha256sum -c --quiet
  unzip -q "$dir/$asset.zip" -d "$dir" && rm "$dir/$asset.zip"
fi
echo "$dir/linux-x64-release"
