#!/usr/bin/env bash
# Build mm-rust and install it for the current user (no root needed):
# the binary, a launcher entry and the icon, under ~/.local by default.
#   packaging/install.sh                 build and install to ~/.local
#   PREFIX=/usr/local packaging/install.sh   (then run as root)
#   packaging/install.sh --no-build      install an existing target/release/mm-rust
#   packaging/install.sh --uninstall     remove what this installs
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX=${PREFIX:-$HOME/.local}
bin=$PREFIX/bin/mm-rust
desktop=$PREFIX/share/applications/mm-rust.desktop
icon=$PREFIX/share/icons/hicolor/scalable/apps/mm-rust.svg

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
}

if [[ ${1:-} == --uninstall ]]; then
  rm -fv "$bin" "$desktop" "$icon"
  refresh
  echo "Kept: settings (~/.config/mm-rust), the saved layout (~/.local/state/mm-rust) and your login (in the keyring)."
  exit 0
fi

if [[ ${1:-} != --no-build ]]; then
  cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
  "$cargo" build --release --locked -p mm-client
fi

install -Dm755 target/release/mm-rust "$bin"
install -Dm644 packaging/mm-rust.desktop "$desktop"
install -Dm644 packaging/mm-rust.svg "$icon"
refresh
echo "Installed $bin"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) echo "Note: $PREFIX/bin isn't on your PATH; the launcher entry still works." ;;
esac
