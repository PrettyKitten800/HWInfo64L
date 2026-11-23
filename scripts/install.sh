#!/usr/bin/env bash
# HWInfo64L cross-distro build & install script.
#
# Usage:
#   ./scripts/install.sh               # Full install (CLI + GTK4 GUI + hardened hwinfo64ld daemon)
#   ./scripts/install.sh --no-daemon   # Unprivileged user-only install into ~/.local/bin (no sudo)
#   ./scripts/install.sh --skip-deps   # Skip automatic distro package dependency installation

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DAEMON=1
SKIP_DEPS=0

for arg in "$@"; do
  case "$arg" in
    --no-daemon) INSTALL_DAEMON=0 ;;
    --skip-deps) SKIP_DEPS=1 ;;
    -h|--help)
      sed -n '2,8p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

install_distro_deps() {
  if [[ "$SKIP_DEPS" -eq 1 ]]; then
    return
  fi

  echo "==> Checking build & runtime dependencies..."
  if command -v apt-get >/dev/null 2>&1; then
    local pkgs=(pkg-config libgtk-4-dev build-essential lm-sensors smartmontools pciutils dmidecode)
    if ! command -v cargo >/dev/null 2>&1; then
      pkgs+=(rustc cargo)
    fi
    sudo apt-get update -qq
    sudo apt-get install -y "${pkgs[@]}"
  elif command -v dnf >/dev/null 2>&1; then
    local pkgs=(pkgconf-pkg-config gtk4-devel gcc lm_sensors smartmontools pciutils dmidecode)
    if ! command -v cargo >/dev/null 2>&1; then
      pkgs+=(rust cargo)
    fi
    sudo dnf install -y "${pkgs[@]}"
  elif command -v pacman >/dev/null 2>&1; then
    local pkgs=(pkgconf gtk4 base-devel lm_sensors smartmontools pciutils dmidecode)
    if ! command -v cargo >/dev/null 2>&1; then
      pkgs+=(rust)
    fi
    sudo pacman -S --needed --noconfirm "${pkgs[@]}"
  elif command -v zypper >/dev/null 2>&1; then
    local pkgs=(pkg-config gtk4-devel gcc sensors smartmontools pciutils dmidecode)
    if ! command -v cargo >/dev/null 2>&1; then
      pkgs+=(rust cargo)
    fi
    sudo zypper install -y "${pkgs[@]}"
  else
    echo "Warning: unrecognized package manager. Ensure rustc/cargo (>=1.80), pkg-config, and GTK4 dev headers are installed."
  fi
}

if [[ "$INSTALL_DAEMON" -eq 1 ]]; then
  install_distro_deps
elif ! command -v cargo >/dev/null 2>&1 || ! pkg-config --exists gtk4 2>/dev/null; then
  echo "Missing cargo or GTK4 dev headers (libgtk-4-dev / gtk4-devel)." >&2
  echo "Install them via your package manager or re-run without --no-daemon." >&2
  exit 1
fi

echo "==> Building HWInfo64L release binaries..."
cd "$REPO_ROOT"
cargo build --release --workspace

if [[ "$INSTALL_DAEMON" -eq 0 ]]; then
  echo "==> Installing unprivileged binaries to ~/.local/bin..."
  mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
  install -m 755 target/release/hwinfo64l "$HOME/.local/bin/"
  install -m 755 target/release/hwinfo64l-gui "$HOME/.local/bin/"
  sed "s|/usr/local/bin/hwinfo64l-gui|$HOME/.local/bin/hwinfo64l-gui|" \
    packaging/desktop/hwinfo64l.desktop > "$HOME/.local/share/applications/hwinfo64l.desktop"
  update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  echo ""
  echo "Installed (user-only mode, no daemon)!"
  echo "Run: $HOME/.local/bin/hwinfo64l-gui  (or $HOME/.local/bin/hwinfo64l summary)"
  exit 0
fi

echo "==> Installing binaries, desktop entry, and hardened hwinfo64ld.service..."
sudo systemctl stop hwinfo64ld.service 2>/dev/null || true
sudo install -m 755 target/release/hwinfo64ld /usr/local/bin/
sudo install -m 755 target/release/hwinfo64l /usr/local/bin/
sudo install -m 755 target/release/hwinfo64l-gui /usr/local/bin/
sudo mkdir -p /usr/local/share/applications /etc/modules-load.d
sudo install -m 644 packaging/desktop/hwinfo64l.desktop /usr/local/share/applications/
sudo install -m 644 packaging/modules-load/hwinfo64l.conf /etc/modules-load.d/hwinfo64l.conf
sudo install -m 644 packaging/systemd/hwinfo64ld.service /etc/systemd/system/hwinfo64ld.service
sudo update-desktop-database /usr/local/share/applications 2>/dev/null || true

sudo modprobe msr 2>/dev/null || true
sudo systemctl daemon-reload
sudo systemctl enable --now hwinfo64ld.service
sleep 1.5

echo ""
echo "==> HWInfo64L installed and running!"
echo "    Launch GUI : hwinfo64l-gui  (or open 'HWInfo64L' from your app launcher)"
echo "    Launch CLI : hwinfo64l summary  |  hwinfo64l watch  |  hwinfo64l probes"
echo "    Uninstall  : ./scripts/uninstall.sh"
echo ""
hwinfo64l probes || true
