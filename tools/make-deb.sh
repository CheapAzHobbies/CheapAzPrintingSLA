#!/usr/bin/env bash
# Build a .deb.
#
# Dependencies are declared rather than bundled: GTK4 and libadwaita are in
# every distribution that is new enough to have them, and a bundled copy of a
# toolkit is a second one to keep patched.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
arch="$(dpkg --print-architecture)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

cargo build --release

install -Dm755 target/release/cheapazsla-gui "$stage/usr/bin/cheapazsla-gui"
install -Dm755 target/release/cheapazsla     "$stage/usr/bin/cheapazsla"
install -Dm644 packaging/com.cheapazhobbies.CheapAzSLA.desktop \
    "$stage/usr/share/applications/com.cheapazhobbies.CheapAzSLA.desktop"
install -Dm644 packaging/com.cheapazhobbies.CheapAzSLA.mime.xml \
    "$stage/usr/share/mime/packages/com.cheapazhobbies.CheapAzSLA.xml"
for png in assets/icons/*.png; do
    size="$(basename "$png" .png)"
    install -Dm644 "$png" \
        "$stage/usr/share/icons/hicolor/${size}x${size}/apps/com.cheapazhobbies.CheapAzSLA.png"
done
install -Dm644 README.md "$stage/usr/share/doc/cheapazsla/README.md"

# Debian wants a copyright file in its own format, pointing at the licence
# text the system already ships rather than shipping a second copy of it.
cat > "$stage/usr/share/doc/cheapazsla/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: CheapAzSLA
Source: https://github.com/CheapAzHobbies/CheapAzPrintingSLA

Files: *
Copyright: CheapAzHobbies
License: GPL-3+

License: GPL-3+
 This program is free software: you can redistribute it and/or modify it under
 the terms of the GNU General Public License as published by the Free Software
 Foundation, either version 3 of the License, or (at your option) any later
 version.
 .
 This program is distributed in the hope that it will be useful, but WITHOUT
 ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
 .
 On Debian systems, the complete text of the GNU General Public License
 version 3 can be found in "/usr/share/common-licenses/GPL-3".
EOF
chmod 644 "$stage/usr/share/doc/cheapazsla/copyright"

# Work out the dependencies from what the binary actually links against, by
# asking which package owns each library, rather than writing a list by hand
# and finding out later that it was wrong.
resolve_deps() {
    # ldd reports the path the loader used, which on this distribution is a
    # symlink under /lib that dpkg has never heard of: the package owns the
    # real file under /usr/lib, usually with the full version in its name. So
    # each path is resolved before being looked up.
    #
    # Some libraries belong to no package at all, and dpkg calls that a
    # failure. That is expected rather than fatal, so each is asked separately.
    #
    # Only what the binary directly needs, from its DT_NEEDED entries. ldd
    # would give the whole transitive closure, and depending on all of that is
    # both noisy and wrong: those come in with the libraries that need them.
    local needed
    needed="$(objdump -p "$stage/usr/bin/cheapazsla-gui" |
        awk '/NEEDED/ {print $2}')"
    for soname in $needed; do
        lib="$(ldd "$stage/usr/bin/cheapazsla-gui" |
            awk -v s="$soname" '$1 == s {print $3}')"
        [ -n "$lib" ] || continue
        real="$(readlink -f "$lib" || echo "$lib")"
        dpkg -S "$real" 2>/dev/null | cut -d: -f1 | tr ',' '\n' | tr -d ' '
    done | sort -u | paste -sd, | sed 's/,/, /g'
}
deps="$(resolve_deps || true)"
if [ -z "$deps" ]; then
    echo "could not resolve dependencies; falling back to the obvious ones" >&2
    deps="libgtk-4-1, libadwaita-1-0, libc6"
fi
echo "depends on: $deps"

mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: cheapazsla
Version: $version
Section: graphics
Priority: optional
Architecture: $arch
Depends: $deps
Maintainer: CheapAzHobbies <cheapazhobbies@users.noreply.github.com>
Homepage: https://github.com/CheapAzHobbies/CheapAzPrintingSLA
Description: Resin print file converter and inspector
 Opens, inspects and converts the file formats resin printers read.
 .
 Reads PrusaSlicer SL1, Elegoo GOO and Chitubox CTB, writes SL1 and GOO, and
 shows what a conversion will drop before it runs. Command line tool included.
EOF

cat > "$stage/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
# So the desktop learns the icon and the file types it now knows how to open.
if [ -x "$(command -v update-mime-database)" ]; then
    update-mime-database /usr/share/mime || true
fi
if [ -x "$(command -v gtk-update-icon-cache)" ]; then
    gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
fi
if [ -x "$(command -v update-desktop-database)" ]; then
    update-desktop-database /usr/share/applications || true
fi
EOF
chmod 755 "$stage/DEBIAN/postinst"
cp "$stage/DEBIAN/postinst" "$stage/DEBIAN/postrm"

out="$root/target/cheapazsla_${version}_${arch}.deb"
fakeroot dpkg-deb --build "$stage" "$out" >/dev/null
echo "built $out"
dpkg-deb --info "$out" | sed -n '2,20p'
