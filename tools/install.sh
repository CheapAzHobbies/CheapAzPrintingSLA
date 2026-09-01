#!/usr/bin/env bash
# Build and install into ~/.local, so the desktop launcher runs the same
# binary as ./target/release.
#
# Without this the two drift: a menu entry pointing at a copy from an earlier
# build looks exactly like the current one having regressed.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"

cargo build --release --manifest-path "$root/Cargo.toml"

install -Dm755 "$root/target/release/cheapazsla-gui" "$prefix/bin/cheapazsla-gui"
install -Dm755 "$root/target/release/cheapazsla" "$prefix/bin/cheapazsla"

for png in "$root"/assets/icons/*.png; do
    size="$(basename "$png" .png)"
    install -Dm644 "$png" \
        "$prefix/share/icons/hicolor/${size}x${size}/apps/com.cheapazhobbies.CheapAzSLA.png"
done

if [ -f "$root/packaging/com.cheapazhobbies.CheapAzSLA.desktop" ]; then
    install -Dm644 "$root/packaging/com.cheapazhobbies.CheapAzSLA.desktop" \
        "$prefix/share/applications/com.cheapazhobbies.CheapAzSLA.desktop"
fi

# The desktop entry says which types it opens; this says what those types are.
# Without it the entry names a type the desktop has never heard of, and
# double-clicking a print file does nothing.
if [ -f "$root/packaging/com.cheapazhobbies.CheapAzSLA.mime.xml" ]; then
    install -Dm644 "$root/packaging/com.cheapazhobbies.CheapAzSLA.mime.xml" \
        "$prefix/share/mime/packages/com.cheapazhobbies.CheapAzSLA.xml"
    update-mime-database "$prefix/share/mime" 2>/dev/null || true
fi

gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" 2>/dev/null || true
update-desktop-database "$prefix/share/applications" 2>/dev/null || true

echo "installed to $prefix/bin/cheapazsla-gui"
