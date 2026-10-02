#!/bin/sh
# Build an installable Ubuntu/Debian package for rgba.
#
# Usage: packaging/build-deb.sh
#
# Produces target/deb/rgba_<version>_<arch>.deb containing the release
# binary, the application menu entry, the hicolor icons and a man page.
# Requires cargo and dpkg-deb (dpkg-dev). Build deps on Ubuntu:
#   sudo apt install build-essential pkg-config libasound2-dev libudev-dev

set -eu
umask 022

cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
PKG=rgba_${VERSION}_${ARCH}
STAGE=target/deb/$PKG

cargo build --release -p rgba

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" \
    "$STAGE/usr/bin" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/doc/rgba" \
    "$STAGE/usr/share/man/man1" \
    "$STAGE/usr/share/icons/hicolor/scalable/apps"

install -s -m 755 target/release/rgba "$STAGE/usr/bin/rgba"
install -m 644 packaging/rgba.desktop "$STAGE/usr/share/applications/rgba.desktop"
install -m 644 packaging/icon/rgba.svg "$STAGE/usr/share/icons/hicolor/scalable/apps/rgba.svg"
for size in 32 48 128 256; do
    mkdir -p "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps"
    install -m 644 "packaging/icon/rgba-$size.png" \
        "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps/rgba.png"
done
gzip -9cn packaging/rgba.1 > "$STAGE/usr/share/man/man1/rgba.1.gz"
chmod 644 "$STAGE/usr/share/man/man1/rgba.1.gz"

# debian/copyright: notice header + full MPL-2.0 text.
{
    echo "rgba — a Rust port of the mGBA emulator"
    echo "Copyright (c) 2013-2021 Jeffrey Pfau (mGBA) and rgba contributors"
    echo "License: MPL-2.0"
    echo
    cat LICENSE
} > "$STAGE/usr/share/doc/rgba/copyright"
chmod 644 "$STAGE/usr/share/doc/rgba/copyright"

# Native-format changelog (one entry per release).
{
    echo "rgba ($VERSION) unstable; urgency=medium"
    echo
    echo "  * Release $VERSION."
    echo
    echo " -- portlandhodl <portlandhodl@users.noreply.github.com>  $(date -R)"
} > "$STAGE/usr/share/doc/rgba/changelog"
gzip -9n "$STAGE/usr/share/doc/rgba/changelog"
chmod 644 "$STAGE/usr/share/doc/rgba/changelog.gz"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: rgba
Version: $VERSION
Section: otherosfs
Priority: optional
Architecture: $ARCH
Maintainer: portlandhodl <portlandhodl@users.noreply.github.com>
Depends: libasound2 (>= 1.0.24), libudev1, libc6 (>= 2.31)
Homepage: https://github.com/portlandhodl/rgba
Description: Game Boy Advance and Game Boy emulator
 rgba is a Rust port of the mGBA emulator, covering the GBA (ARM7TDMI)
 and GB (SM83) consoles with cycle-accurate timing semantics, a full
 debugger, cheats, savestates and link-cable emulation. MPL-2.0.
EOF

dpkg-deb --root-owner-group --build "$STAGE"
echo "built target/deb/$PKG.deb"
