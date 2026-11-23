#!/usr/bin/env bash
# Build a Debian/Ubuntu .deb package and portable Linux .tar.gz bundle in dist/.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
ARCH="$(dpkg --print-architecture 2>/dev/null || uname -m)"
DIST_DIR="$REPO_ROOT/dist"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> Building release binaries (v${VERSION}, ${ARCH})..."
cargo build --release --workspace

mkdir -p "$DIST_DIR"

# --- 1. Build .deb package ---
PKG_ROOT="$STAGE/hwinfo64l_${VERSION}_${ARCH}"
mkdir -p \
  "$PKG_ROOT/DEBIAN" \
  "$PKG_ROOT/usr/bin" \
  "$PKG_ROOT/usr/lib/systemd/system" \
  "$PKG_ROOT/usr/lib/modules-load.d" \
  "$PKG_ROOT/usr/share/applications" \
  "$PKG_ROOT/usr/share/doc/hwinfo64l" \
  "$PKG_ROOT/usr/share/hwinfo64l/examples/plugins"

install -m 755 target/release/hwinfo64ld "$PKG_ROOT/usr/bin/"
install -m 755 target/release/hwinfo64l "$PKG_ROOT/usr/bin/"
install -m 755 target/release/hwinfo64l-gui "$PKG_ROOT/usr/bin/"

sed 's|/usr/local/bin/hwinfo64ld|/usr/bin/hwinfo64ld|g' \
  packaging/systemd/hwinfo64ld.service > "$PKG_ROOT/usr/lib/systemd/system/hwinfo64ld.service"
chmod 644 "$PKG_ROOT/usr/lib/systemd/system/hwinfo64ld.service"

sed 's|/usr/local/bin/hwinfo64l-gui|/usr/bin/hwinfo64l-gui|g' \
  packaging/desktop/hwinfo64l.desktop > "$PKG_ROOT/usr/share/applications/hwinfo64l.desktop"
chmod 644 "$PKG_ROOT/usr/share/applications/hwinfo64l.desktop"

install -m 644 packaging/modules-load/hwinfo64l.conf "$PKG_ROOT/usr/lib/modules-load.d/hwinfo64l.conf"
install -m 644 LICENSE README.md docs/ARCHITECTURE.md docs/PROTOCOL.md docs/PLUGIN_GUIDE.md "$PKG_ROOT/usr/share/doc/hwinfo64l/"
install -m 755 examples/plugins/loadavg_plugin.py "$PKG_ROOT/usr/share/hwinfo64l/examples/plugins/"
install -m 755 plugins/klipper-moonraker/klipper_moonraker.py "$PKG_ROOT/usr/share/hwinfo64l/examples/plugins/"

INSTALLED_KB="$(du -sk "$PKG_ROOT" | awk '{print $1}')"

cat > "$PKG_ROOT/DEBIAN/control" <<EOF
Package: hwinfo64l
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${INSTALLED_KB}
Depends: libgtk-4-1, libc6
Recommends: lm-sensors, smartmontools, pciutils, dmidecode, intel-gpu-tools
Maintainer: HWInfo64L Contributors <https://github.com/PrettyKitten800/HWInfo64L>
Homepage: https://github.com/PrettyKitten800/HWInfo64L
Description: HWiNFO64-style hardware inventory and live sensor monitor for Linux
 Fault-isolated Rust telemetry daemon (hwinfo64ld), CLI (hwinfo64l), and native
 GTK4 desktop UI (hwinfo64l-gui) with zero open network ports and drop-in
 NDJSON plugin extensibility.
EOF

cat > "$PKG_ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "configure" ]; then
  modprobe msr 2>/dev/null || true
  if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    systemctl enable --now hwinfo64ld.service || true
  fi
  update-desktop-database /usr/share/applications 2>/dev/null || true
fi
EOF
chmod 755 "$PKG_ROOT/DEBIAN/postinst"

cat > "$PKG_ROOT/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "remove" ] && [ -d /run/systemd/system ]; then
  systemctl disable --now hwinfo64ld.service || true
fi
EOF
chmod 755 "$PKG_ROOT/DEBIAN/prerm"

cat > "$PKG_ROOT/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
fi
EOF
chmod 755 "$PKG_ROOT/DEBIAN/postrm"

DEB_PATH="$DIST_DIR/hwinfo64l_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$PKG_ROOT" "$DEB_PATH"

# --- 2. Build portable .tar.gz bundle (for Fedora/Arch/any distro) ---
TAR_NAME="hwinfo64l-${VERSION}-linux-${ARCH}"
TAR_ROOT="$STAGE/$TAR_NAME"
mkdir -p "$TAR_ROOT/bin" "$TAR_ROOT/packaging" "$TAR_ROOT/plugins"
install -m 755 target/release/hwinfo64ld target/release/hwinfo64l target/release/hwinfo64l-gui "$TAR_ROOT/bin/"
install -m 644 packaging/systemd/hwinfo64ld.service packaging/desktop/hwinfo64l.desktop packaging/modules-load/hwinfo64l.conf "$TAR_ROOT/packaging/"
install -m 755 examples/plugins/loadavg_plugin.py plugins/klipper-moonraker/klipper_moonraker.py "$TAR_ROOT/plugins/"
install -m 644 LICENSE README.md "$TAR_ROOT/"

cat > "$TAR_ROOT/install-prebuilt.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sudo systemctl stop hwinfo64ld.service 2>/dev/null || true
sudo install -m 755 "$DIR/bin/hwinfo64ld" "$DIR/bin/hwinfo64l" "$DIR/bin/hwinfo64l-gui" /usr/local/bin/
sudo mkdir -p /usr/local/share/applications /etc/modules-load.d
sudo install -m 644 "$DIR/packaging/hwinfo64l.desktop" /usr/local/share/applications/
sudo install -m 644 "$DIR/packaging/hwinfo64l.conf" /etc/modules-load.d/hwinfo64l.conf
sudo install -m 644 "$DIR/packaging/hwinfo64ld.service" /etc/systemd/system/hwinfo64ld.service
sudo modprobe msr 2>/dev/null || true
sudo systemctl daemon-reload
sudo systemctl enable --now hwinfo64ld.service
echo "HWInfo64L installed! Launch with: hwinfo64l-gui"
EOF
chmod 755 "$TAR_ROOT/install-prebuilt.sh"

TAR_PATH="$DIST_DIR/${TAR_NAME}.tar.gz"
tar -C "$STAGE" -czf "$TAR_PATH" "$TAR_NAME"

echo ""
echo "==> Packages built:"
ls -lh "$DEB_PATH" "$TAR_PATH"
