#!/bin/bash
# Install the application icon into the user's hicolor theme.
#
# Run from a checkout: packaging/install-icons.sh
# Distribution packages should install the same files under /usr/share instead.
set -euo pipefail
cd "$(dirname "$0")/.."

APP=com.cheapazhobbies.CheapAzSLA
PREFIX=${PREFIX:-$HOME/.local/share}

for size in 512 256 128 96 64 48 32 24 16; do
    src="assets/icons/${size}.png"
    [ -f "$src" ] || continue
    install -Dm644 "$src" "$PREFIX/icons/hicolor/${size}x${size}/apps/$APP.png"
done

install -Dm644 "packaging/$APP.desktop" "$PREFIX/applications/$APP.desktop"

# Both are best-effort: a missing cache is rebuilt by the desktop eventually.
gtk-update-icon-cache -f -t "$PREFIX/icons/hicolor" 2>/dev/null || true
update-desktop-database "$PREFIX/applications" 2>/dev/null || true

echo "Installed the icon and desktop entry under $PREFIX"
