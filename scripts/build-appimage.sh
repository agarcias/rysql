#!/usr/bin/env bash
# Build a portable RySQL AppImage. Builds in release mode, assembles an
# AppDir, then runs appimagetool. Downloads appimagetool to target/ if
# it isn't on PATH.
#
# Usage: scripts/build-appimage.sh [version]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(awk -F\" '/^version =/ {print $2; exit}' Cargo.toml)}"
ARCH="${ARCH:-x86_64}"
OUT_DIR="$ROOT/target/appimage"
APPDIR="$OUT_DIR/RySQL.AppDir"
TARGET_BIN="$ROOT/target/release/rysql"

echo "==> Building rysql v$VERSION ($ARCH) in release mode"
cargo build --release --locked --bin rysql

mkdir -p "$OUT_DIR"
rm -rf "$APPDIR"
mkdir -p \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/scalable/apps" \
    "$APPDIR/usr/share/metainfo"

install -m755 "$TARGET_BIN" "$APPDIR/usr/bin/rysql"
install -m644 "$ROOT/packaging/linux/rysql.desktop"       "$APPDIR/usr/share/applications/"
install -m644 "$ROOT/packaging/linux/rysql.svg"           "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
install -m644 "$ROOT/packaging/linux/io.rysql.RySQL.metainfo.xml" "$APPDIR/usr/share/metainfo/"

# AppImage expects these at the AppDir root.
install -m644 "$ROOT/packaging/linux/rysql.desktop" "$APPDIR/rysql.desktop"
install -m644 "$ROOT/packaging/linux/rysql.svg"     "$APPDIR/rysql.svg"
ln -sf rysql.svg "$APPDIR/.DirIcon"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/rysql" "$@"
EOF
chmod +x "$APPDIR/AppRun"

APPIMAGETOOL="$(command -v appimagetool || true)"
if [ -z "$APPIMAGETOOL" ]; then
    APPIMAGETOOL="$OUT_DIR/appimagetool-${ARCH}.AppImage"
    if [ ! -x "$APPIMAGETOOL" ]; then
        echo "==> Downloading appimagetool"
        curl -fsSL --output "$APPIMAGETOOL" \
            "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
        chmod +x "$APPIMAGETOOL"
    fi
fi

OUTPUT="$OUT_DIR/RySQL-${VERSION}-${ARCH}.AppImage"
echo "==> Running appimagetool"
ARCH="$ARCH" "$APPIMAGETOOL" --no-appstream "$APPDIR" "$OUTPUT"
echo "==> Built $OUTPUT"
