#!/usr/bin/env bash
# Fetches libwebrtc for this machine (or the triple given: linux-x64,
# linux-arm64, mac-x64, mac-arm64) and checks its SHA-256. .cargo/config.toml
# points webrtc-sys at .webrtc/current, so its build script never downloads
# an unchecked copy itself. Needed to build dm-call (calls).
#
# Our own builds (tag webrtc-89d790b-free, made by dev/build-webrtc.sh and
# the libwebrtc workflow): libwebrtc without H.264 or FFmpeg, which
# dev/check-webrtc-licence.sh checks before every release build.
set -euo pipefail
base=webrtc-89d790b  # webrtc-sys-build 0.3.20's WEBRTC_TAG
triple=${1:-}
if [[ -z $triple ]]; then
  case "$(uname -s)" in Linux) os=linux ;; Darwin) os=mac ;; *) echo "no libwebrtc for $(uname -s)" >&2; exit 1 ;; esac
  case "$(uname -m)" in x86_64|amd64) arch=x64 ;; aarch64|arm64) arch=arm64 ;; *) echo "no libwebrtc for $(uname -m)" >&2; exit 1 ;; esac
  triple=$os-$arch
fi
# Where each comes from, and the SHA-256 of its archive.
ours=karstandsons/darkmatter
case $triple in
  linux-x64) tag=$base-free repo=$ours sha=db99ca95f37820e14c47be5f2f71a746ebdc1e58c692cf5b63789ecb9402dc46 ;;
  linux-arm64) tag=$base-free repo=$ours sha=c32585bfe402f0eb1883dcff365c4391499d9869a818b2c0751edcca74607a2c ;;
  mac-x64) tag=$base-free repo=$ours sha=0629c7f8eaa8a53f62e0ecb2bfc922ab50df23b6354a005bb7cbbe051e948490 ;;
  mac-arm64) tag=$base-free repo=$ours sha=09db30168d536d54c72ed88e39f48f027dbf1e9b98f9c43c1cac4745acb588e5 ;;
  *) echo "unknown triple $triple" >&2; exit 1 ;;
esac
asset=webrtc-$triple-release.zip
root=$(cd "$(dirname "$0")/.." && pwd)/.webrtc
dir=$root/$tag/$triple
if [[ ! -d $dir/$triple-release ]]; then
  mkdir -p "$dir"
  zip=$dir/webrtc.zip
  if [[ $repo == "$ours" ]] && command -v gh >/dev/null; then
    # Works while the repository is private, with gh's (or CI's) token.
    gh release download "$tag" -R "$repo" -p "$asset" -O "$zip" --clobber
  else
    curl -sSfL --retry 3 -o "$zip" "https://github.com/$repo/releases/download/$tag/$asset"
  fi
  if command -v sha256sum >/dev/null; then echo "$sha  $zip" | sha256sum -c --quiet; else echo "$sha  $zip" | shasum -a 256 -c --quiet; fi
  unzip -q "$zip" -d "$dir" && rm "$zip"
fi
ln -sfn "$dir/$triple-release" "$root/current"
echo "$root/current -> $dir/$triple-release"
