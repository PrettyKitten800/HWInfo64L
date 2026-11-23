#!/usr/bin/env bash
# Cleanly uninstall HWInfo64L binaries, systemd service, and desktop entry.

set -euo pipefail

echo "==> Removing HWInfo64L..."

if systemctl list-unit-files hwinfo64ld.service >/dev/null 2>&1; then
  sudo systemctl disable --now hwinfo64ld.service 2>/dev/null || true
fi

sudo rm -f \
  /etc/systemd/system/hwinfo64ld.service \
  /etc/modules-load.d/hwinfo64l.conf \
  /usr/local/bin/hwinfo64ld \
  /usr/local/bin/hwinfo64l \
  /usr/local/bin/hwinfo64l-gui \
  /usr/local/share/applications/hwinfo64l.desktop

sudo systemctl daemon-reload 2>/dev/null || true
sudo update-desktop-database /usr/local/share/applications 2>/dev/null || true

# Also remove any user-local install if present
rm -f \
  "$HOME/.local/bin/hwinfo64l" \
  "$HOME/.local/bin/hwinfo64l-gui" \
  "$HOME/.local/share/applications/hwinfo64l.desktop"

echo "HWInfo64L removed cleanly. (User plugins in ~/.config/hwinfo64l/ were left untouched.)"
