#!/usr/bin/env bash
# Builds a .deb package of the Notes app:  ./packaging/build-deb.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# ---- edit this ---------------------------------------------------------
MAINTAINER="Naim zannatulnaim09@gmail.com>"
# ------------------------------------------------------------------------

PKG=notes-app
APP_ID=com.example.Notes
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(dpkg --print-architecture)
STAGE="target/deb/${PKG}_${VERSION}_${ARCH}"
OUT="target/deb/${PKG}_${VERSION}_${ARCH}.deb"

echo "==> Building release binary"
cargo build --release

echo "==> Staging package files"
rm -rf "$STAGE"
install -Dm755 "target/release/$PKG"           "$STAGE/usr/bin/$PKG"
install -Dm644 "packaging/$APP_ID.desktop"     "$STAGE/usr/share/applications/$APP_ID.desktop"
install -Dm644 "packaging/$APP_ID.svg"         "$STAGE/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 packaging/copyright             "$STAGE/usr/share/doc/$PKG/copyright"

# man page and changelog (compressed, as Debian policy expects)
install -Dm644 packaging/$PKG.1 "$STAGE/usr/share/man/man1/$PKG.1"
gzip -9n "$STAGE/usr/share/man/man1/$PKG.1"
mkdir -p "$STAGE/usr/share/doc/$PKG"
{
  echo "$PKG ($VERSION) unstable; urgency=low"
  echo
  echo "  * Release $VERSION."
  echo
  echo " -- $MAINTAINER  $(date -R)"
} | gzip -9n > "$STAGE/usr/share/doc/$PKG/changelog.gz"

mkdir -p "$STAGE/DEBIAN"
SIZE=$(du -ks "$STAGE" | cut -f1)
cat > "$STAGE/DEBIAN/control" << EOF
Package: $PKG
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libgtk-4-1 (>= 4.14), libadwaita-1-0 (>= 1.5), libc6
Installed-Size: $SIZE
Maintainer: $MAINTAINER
Description: Modern, minimal note-taking app
 Markdown notes with live styling, pinned notes, #tags, search and a
 light/dark theme. Notes are stored as plain .md files in
 ~/.local/share/notes-app/notes.
EOF

# dpkg is picky: every directory must be 755
find "$STAGE" -type d -exec chmod 755 {} +

echo "==> Creating $OUT"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT"
echo
echo "Done. Install with:  sudo apt install ./$OUT"
