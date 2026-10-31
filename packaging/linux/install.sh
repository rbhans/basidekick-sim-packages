#!/bin/sh
# Installs BASidekick Building Sim for this user: the binaries in
# ~/.local/bin, the launcher and icon where desktops look for them.
set -e
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
SHARE="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$BIN" "$SHARE/applications" "$SHARE/icons/hicolor/256x256/apps"
install -m 755 basidekick-sim basidekick-sim-cli "$BIN/"
install -m 644 com.basidekick.buildingsim.desktop "$SHARE/applications/"
install -m 644 com.basidekick.buildingsim.png "$SHARE/icons/hicolor/256x256/apps/"
command -v update-desktop-database >/dev/null && update-desktop-database "$SHARE/applications" || true
echo "Installed to $BIN. If basidekick-sim isn't found, add $BIN to your PATH."
