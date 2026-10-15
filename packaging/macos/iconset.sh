#!/usr/bin/env bash
# Render the macOS icon sizes from the SVG (needs rsvg-convert or ImageMagick):
#   packaging/macos/iconset.sh <out dir>    makes <out>/darkmatter.iconset/
# The macOS build turns that into darkmatter.icns with `iconutil`.
set -euo pipefail
svg=$(cd "$(dirname "$0")/.." && pwd)/darkmatter.svg
out=$1/darkmatter.iconset
mkdir -p "$out"
# rsvg-convert (librsvg) draws the gradients best; ImageMagick also works.
render() {
  if command -v rsvg-convert >/dev/null; then
    rsvg-convert -w "$1" -h "$1" "$svg" -o "$2"
  else
    "$(command -v magick || command -v convert)" -background none -density 600 "$svg" -resize "$1x$1" "$2"
  fi
}
for s in 16 32 128 256 512; do
  render "$s" "$out/icon_${s}x${s}.png"
  render "$((s * 2))" "$out/icon_${s}x${s}@2x.png"
done
echo "$out"
