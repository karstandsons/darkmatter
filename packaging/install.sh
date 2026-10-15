#!/usr/bin/env bash
# Build darkmatter and install it for the current user (no root needed):
# the binary, a launcher entry and the icon, under ~/.local by default.
#   packaging/install.sh                 build and install to ~/.local
#   PREFIX=/usr/local packaging/install.sh   (then run as root)
#   packaging/install.sh --no-build      install an existing target/release/darkmatter
#   packaging/install.sh --uninstall     remove what this installs
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX=${PREFIX:-$HOME/.local}
bin=$PREFIX/bin/darkmatter
desktop=$PREFIX/share/applications/darkmatter.desktop
icon=$PREFIX/share/icons/hicolor/scalable/apps/darkmatter.svg

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
}

if [[ ${1:-} == --uninstall ]]; then
  rm -fv "$bin" "$desktop" "$icon"
  refresh
  echo "Kept: settings (~/.config/darkmatter), the saved layout (~/.local/state/darkmatter) and your login (in the keyring)."
  exit 0
fi

if [[ ${1:-} != --no-build ]]; then
  cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
  "$cargo" build --release --locked -p darkmatter
fi

install -Dm755 target/release/darkmatter "$bin"
install -Dm644 packaging/darkmatter.desktop "$desktop"
install -Dm644 packaging/darkmatter.svg "$icon"
refresh
echo "Installed $bin"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) echo "Note: $PREFIX/bin isn't on your PATH; the launcher entry still works." ;;
esac
