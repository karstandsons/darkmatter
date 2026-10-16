#!/usr/bin/env bash
# Install Darkmatter for the current user (no root needed): the binary, a
# launcher entry and the icon, under ~/.local by default.
#
# From a release tarball (the binary is next to this script):
#   ./install.sh                         install to ~/.local
# From a source checkout (builds first):
#   packaging/install.sh                 build and install to ~/.local
#   packaging/install.sh --no-build      install an existing target/release/darkmatter
# Either way:
#   PREFIX=/usr/local ./install.sh       somewhere else (then run as root)
#   ./install.sh --uninstall             remove what this installs
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
PREFIX=${PREFIX:-$HOME/.local}
bin=$PREFIX/bin/darkmatter
desktop=$PREFIX/share/applications/darkmatter.desktop
icon=$PREFIX/share/icons/hicolor/scalable/apps/darkmatter.svg
plugins=$PREFIX/share/darkmatter/plugins

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
}

if [[ ${1:-} == --uninstall ]]; then
  rm -fv "$bin" "$desktop" "$icon"
  rm -rf "$plugins"
  refresh
  echo "Kept: settings (~/.config/darkmatter), the saved layout (~/.local/state/darkmatter) and your login (in the keyring)."
  exit 0
fi

if [[ -x $here/darkmatter ]]; then
  # A release tarball: everything is next to this script.
  src_bin=$here/darkmatter src_desktop=$here/darkmatter.desktop src_icon=$here/darkmatter.svg src_plugins=$here/plugins
else
  root=$(cd "$here/.." && pwd)
  if [[ ${1:-} != --no-build ]]; then
    cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
    (cd "$root" && "$cargo" build --release --locked -p darkmatter)
  fi
  src_bin=$root/target/release/darkmatter src_desktop=$here/darkmatter.desktop src_icon=$here/darkmatter.svg src_plugins=$root/plugins
fi

install -Dm755 "$src_bin" "$bin"
install -Dm644 "$src_desktop" "$desktop"
install -Dm644 "$src_icon" "$icon"
# The built-in plugins (meme, gif, kaomoji, quote), replaced whole.
rm -rf "$plugins"
mkdir -p "$plugins"
cp -r "$src_plugins/." "$plugins/"
find "$plugins" \( -name __pycache__ -o -name .gitignore \) -prune -exec rm -rf {} +
refresh
echo "Installed $bin ($("$bin" --version))"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) echo "Note: $PREFIX/bin isn't on your PATH; the launcher entry still works." ;;
esac
