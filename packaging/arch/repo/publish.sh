#!/usr/bin/env bash
# Build and sign the `darkmatter` package for the Karst & Sons pacman repo and
# add it to the repo database. Runs as root in an archlinux container (CI).
#   packaging/arch/repo/publish.sh <version> <dir with release tarballs> <site checkout>
# Env: PACMAN_SIGNING_KEY, the armored private signing key.
# The site checkout (the repo's gh-pages) is updated in place; pushing it is
# up to the caller.
set -euo pipefail
version=$1 dist=$(realpath "$2") site=$(realpath "$3")
here=$(cd "$(dirname "$0")" && pwd)

printf '%s\n' "$PACMAN_SIGNING_KEY" | gpg --batch --quiet --import
key=$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')
id -u builder &>/dev/null || useradd -m builder   # makepkg won't run as root

for arch in x86_64 aarch64; do
  tarball=$dist/darkmatter-$version-$arch-linux.tar.gz
  [[ -f $tarball ]] || { echo "no $arch build; skipping"; continue; }
  build=$(mktemp -d)
  sed "s/^pkgver=.*/pkgver=$version/" "$here/PKGBUILD" > "$build/PKGBUILD"
  cp "$tarball" "$build/"
  # The binary is already built, so packaging for another arch is just a label.
  sed "s/^CARCH=.*/CARCH=\"$arch\"/" /etc/makepkg.conf > "$build/makepkg.conf"
  chown -R builder "$build"
  su builder -c "cd '$build' && makepkg --config makepkg.conf --nodeps --noconfirm -f"
  pkg=$(ls "$build"/darkmatter-"$version"-*-"$arch".pkg.tar.zst)
  gpg --batch --yes --detach-sign --no-armor -u "$key" "$pkg"

  mkdir -p "$site/$arch"
  cp "$pkg" "$pkg.sig" "$site/$arch/"
  # --remove drops the previous version's files; -s signs the database.
  repo-add --remove -s -k "$key" "$site/$arch/karstandsons.db.tar.gz" "$site/$arch/$(basename "$pkg")"
  rm -f "$site/$arch/.keep"
done

# GitHub Pages serves files, not symlinks: make the db links real copies.
find "$site" -type l -print0 | while IFS= read -r -d '' link; do
  cp --remove-destination "$(readlink -f "$link")" "$link"
done
ls -la "$site"/*/
