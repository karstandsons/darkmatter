#!/usr/bin/env bash
# Fetches LiveKit's prebuilt libwebrtc for this machine (or the triple
# given: linux-x64, linux-arm64, mac-x64, mac-arm64) and checks it against
# the SHA-256 GitHub published for the release asset. .cargo/config.toml
# points webrtc-sys at .webrtc/current, so its build script never downloads
# an unchecked copy itself. Needed to build dm-call (calls).
set -euo pipefail
tag=webrtc-89d790b  # webrtc-sys-build 0.3.20's WEBRTC_TAG
triple=${1:-}
if [[ -z $triple ]]; then
  case "$(uname -s)" in Linux) os=linux ;; Darwin) os=mac ;; *) echo "no prebuilt for $(uname -s)" >&2; exit 1 ;; esac
  case "$(uname -m)" in x86_64|amd64) arch=x64 ;; aarch64|arm64) arch=arm64 ;; *) echo "no prebuilt for $(uname -m)" >&2; exit 1 ;; esac
  triple=$os-$arch
fi
# SHA-256 of each release asset, as GitHub publishes them.
case $triple in
  linux-x64) sha=b167adad5291cea0e4d66a0454d9d52d2ad714e6b0ed70f4410317d3ebde70c5 ;;
  linux-arm64) sha=f716b10eade18dd11b03b9f93b50cee2ca2eec73b975a63f2ae9ea2a35706420 ;;
  mac-x64) sha=04eea79951eaefc1054099464f949f6dcfab9bc7460c16004811abc4f9ec15d4 ;;
  mac-arm64) sha=9f25fea48588deac18d68e120d18b33af7ef10f921f74b7c57f66b642262c348 ;;
  *) echo "unknown triple $triple" >&2; exit 1 ;;
esac
root=$(cd "$(dirname "$0")/.." && pwd)/.webrtc
dir=$root/$tag/$triple
if [[ ! -d $dir/$triple-release ]]; then
  mkdir -p "$dir"
  zip=$dir/webrtc.zip
  curl -sSfL --retry 3 -o "$zip" "https://github.com/livekit/rust-sdks/releases/download/$tag/webrtc-$triple-release.zip"
  if command -v sha256sum >/dev/null; then echo "$sha  $zip" | sha256sum -c --quiet; else echo "$sha  $zip" | shasum -a 256 -c --quiet; fi
  unzip -q "$zip" -d "$dir" && rm "$zip"
fi
ln -sfn "$dir/$triple-release" "$root/current"
echo "$root/current -> $dir/$triple-release"
