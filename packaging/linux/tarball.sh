#!/usr/bin/env bash
# Pack a built Linux binary into a release tarball:
#   packaging/linux/tarball.sh <binary> <arch, e.g. x86_64> [out dir (default dist)]
# Makes <out>/darkmatter-<version>-<arch>-linux.tar.gz holding the binary, the
# launcher entry, the icon, install.sh, the README and the licenses.
set -euo pipefail
bin=$1 arch=$2 out=${3:-dist}
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$("$bin" --version | awk '{print $2}')
name=darkmatter-$version-$arch-linux
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
install -m755 "$bin" "$stage/$name/darkmatter"
install -m755 "$root/packaging/install.sh" "$stage/$name/install.sh"
install -m644 "$root/packaging/darkmatter.desktop" "$root/packaging/darkmatter.svg" \
  "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$stage/$name/"
mkdir -p "$out"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
