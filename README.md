# HWInfo64L

**HWiNFO64-style hardware inventory and real-time sensor monitoring for Linux — built in Rust.**

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

## Why HWInfo64L?

Linux has great low-level telemetry scattered across dozens of interfaces (`hwmon`, Intel RAPL/`powercap`, MSRs, `dmidecode`/SMBIOS, `smartctl`, `drm`/`intel_gpu_top`, `amdgpu`, NVML, `/proc`, `sysfs`, `zswap`), but no single unified desktop app with a rock-solid fault-isolated core, live **Current / Min / Max / Avg** sensor views, hardware summary/tree, CSV logging, and open extensibility.

## Architecture Highlights

- **Never-fails Rust core (`hwinfo64l-core` + `hwinfo64ld`)**: Zero-panic policy, bounded ring buffers (flat ~10–15 MiB RAM forever), per-probe timeouts, and circuit breakers so a hung drive or bad plugin can never freeze or crash the app.
- **Split privilege model**: A tiny, strictly **read-only** hardened systemd daemon (`hwinfo64ld`) handles root-only telemetry (SMART, SMBIOS/DMI, MSRs) and streams data over `/run/hwinfo64l/hwinfo64l.sock`. If the daemon isn't installed, the CLI/GUI automatically falls back to unprivileged in-process probing.
- **Native GTK4 desktop GUI (`hwinfo64l-gui`)**: Dense, HWiNFO64-inspired Summary, Hardware Tree, and Live Sensors views with sparklines, pop-out charts, and CSV logging.
- **Zero open network ports**: Everything communicates strictly over a local Unix domain socket (`/run/hwinfo64l/hwinfo64l.sock`) — no built-in web server or remote listener.
- **Zero-recompile extensibility**:
  - **Drop-in probe plugins** in any language (Python, Bash, Rust, Go, C) via NDJSON over stdin/stdout.
  - **Open local Unix socket JSON-RPC 2.0 API** so Waybar, Polybar, local scripts, or custom desktop widgets can connect directly.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/PROTOCOL.md](docs/PROTOCOL.md), and [docs/PLUGIN_GUIDE.md](docs/PLUGIN_GUIDE.md) for the complete design and specifications.

## Workspace Layout

- `crates/hwinfo64l-protocol` — Versioned `v1` data schema, sensor IDs, JSON-RPC 2.0 & NDJSON plugin contracts
- `crates/hwinfo64l-core` — Fault-isolated probe supervisor, bounded ring-buffer store, min/max/avg engine, plugin host, CSV logger
- `crates/hwinfo64l-probes` — Native Linux collectors (`hwmon`, `cpufreq`, `cpu_stat`, `rapl`, `msr` VID & Performance Limit Reasons, `memory`, `zswap`, `dmi`, `buses` PCIe/USB, `gpu_drm`, `intel_gpu_top`, `nvidia_gpu`, `disk_io`, `smart`, `network`, `battery`)
- `crates/hwinfo64ld` — Hardened read-only systemd collector daemon (`AF_UNIX` only)
- `crates/hwinfo64l-cli` — Terminal summary, live watch, JSON dump, and CSV logger
- `crates/hwinfo64l-gui` — Native GTK4 desktop UI (Summary, Hardware Tree, Live Sensors + Sparklines, Pop-out Graphs, CSV Logging, Probes & Plugins)

## Install & Quick Start

### Option 1 — Prebuilt `.deb` (Debian / Ubuntu / Mint / Pop!_OS)
Download `hwinfo64l_<version>_amd64.deb` from the [GitHub Releases](https://github.com/PrettyKitten800/HWInfo64L/releases) page and run:
```bash
sudo apt install ./hwinfo64l_*_amd64.deb
hwinfo64l-gui
```

### Option 2 — One-Command Build & Install (Debian / Ubuntu / Fedora / Arch / openSUSE)
```bash
git clone https://github.com/PrettyKitten800/HWInfo64L.git
cd HWInfo64L
./scripts/install.sh
```
Want to test **without installing a root daemon** (unprivileged `~/.local/bin` only)?
```bash
./scripts/install.sh --no-daemon
```
Cleanly remove everything at any time:
```bash
./scripts/uninstall.sh
```

### CLI & GUI Usage
```bash
hwinfo64l-gui              # Launch native GTK4 desktop app (also in your app menu)
hwinfo64l summary          # Hardware summary
hwinfo64l watch            # Live terminal sensor table
hwinfo64l probes           # Diagnostic health & poll latency for every probe/plugin
hwinfo64l log-csv out.csv  # HWiNFO64-compatible CSV logging
```

### Tester Feedback Tip
If a sensor or hardware category is missing on your machine, run `hwinfo64l probes` and `hwinfo64l summary` and share the output in an issue — every probe reports its exact `Healthy` / `Unsupported` / `RequiresPrivilege` / `Degraded` reason!

## License

GNU General Public License v3.0 — see [LICENSE](LICENSE).
