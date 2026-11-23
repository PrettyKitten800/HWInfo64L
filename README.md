# HWInfo64L

**HWiNFO64-style hardware inventory and real-time sensor monitoring for Linux — built in Rust.**

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

## Why HWInfo64L?

Linux has great low-level telemetry scattered across dozens of interfaces (`hwmon`, Intel RAPL/`powercap`, MSRs, `dmidecode`/SMBIOS, `smartctl`, `drm`/`intel_gpu_top`, `amdgpu`, NVML, `/proc`, `sysfs`, `zswap`), but no single unified desktop app with a rock-solid fault-isolated core, live **Current / Min / Max / Avg** sensor views, hardware summary/tree, CSV logging, and open extensibility.

## Architecture Highlights

- **Never-fails Rust core (`hwinfo64l-core` + `hwinfo64ld`)**: Zero-panic policy, bounded ring buffers (flat ~10–15 MiB RAM forever), per-probe timeouts, and circuit breakers so a hung drive or bad plugin can never freeze or crash the app.
- **Split privilege model**: A tiny, strictly **read-only** hardened systemd daemon (`hwinfo64ld`) handles root-only telemetry (SMART, SMBIOS/DMI, MSRs) and streams data over `/run/hwinfo64l/hwinfo64l.sock`. If the daemon isn't installed, the CLI/GUI automatically falls back to unprivileged in-process probing.
- **Native GTK4 desktop GUI (`hwinfo64l-gui`)**: Dense, HWiNFO64-inspired Summary, Hardware Tree, and Live Sensors views with sparklines, pop-out charts, and CSV logging.
- **Zero-recompile extensibility**:
  - **Drop-in probe plugins** in any language (Python, Bash, Rust, Go, C) via NDJSON over stdin/stdout.
  - **Open Unix socket JSON-RPC 2.0 API** so Waybar, Polybar, Home Assistant, Prometheus, Klipper, or a LAN Web UI can connect directly.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/PROTOCOL.md](docs/PROTOCOL.md) for the complete design and wire specifications.

## Workspace Layout

- `crates/hwinfo64l-protocol` — Versioned `v1` data schema, sensor IDs, JSON-RPC 2.0 & NDJSON plugin contracts
- `crates/hwinfo64l-core` — Fault-isolated probe supervisor, bounded ring-buffer store, min/max/avg engine, plugin host, CSV logger
- `crates/hwinfo64l-probes` — Native Linux collectors (`hwmon`, `cpufreq`, `cpu_stat`, `rapl`, `msr`, `memory`, `zswap`, `dmi`, `pci`, `gpu`, `smart`, `net`)
- `crates/hwinfo64ld` — Hardened read-only systemd collector daemon
- `crates/hwinfo64l-cli` — Terminal summary, live watch, JSON dump, and CSV logger
- `crates/hwinfo64l-gui` — Native GTK4 desktop UI *(Phase 2)*

## License

GNU General Public License v3.0 — see [LICENSE](LICENSE).
