# HWInfo64L — System Architecture

> **Goal:** A rock-solid, never-fails, GPL-3.0 Linux equivalent to HWiNFO64 whose core never needs to be rewritten — where new hardware probes, external integrations, and UIs plug in cleanly without recompiling the core.

## 1. Layered Split Architecture

HWInfo64L separates **hardware probing**, **state & statistics**, **transport**, and **user interfaces** behind a versioned, additive-only contract (`v1`):

```text
+-----------------------------------------------------------------------+
| hwinfo64ld (Read-Only Systemd Daemon, Minimal Capabilities)           |
|  - Built-in Native Probes (sysfs, procfs, hwmon, RAPL, DMI, MSR, ...) |
|  - System Plugins (/usr/lib/hwinfo64l/plugins — NDJSON)               |
|  - Probe Supervisor (Per-probe timeouts + Circuit Breakers)           |
|  - Bounded Ring-Buffer Store (Current / Min / Max / Avg)              |
|  - Unix Socket Server (/run/hwinfo64l/hwinfo64l.sock, JSON-RPC 2.0)   |
+-----------------------------------+-----------------------------------+
                                    |
         +--------------------------+--------------------------+
         |                          |                          |
   hwinfo64l-gui              hwinfo64l-cli             Local Clients
   (Native GTK4)           (watch / summary /        (Waybar, Polybar,
   + User Plugins            json / csv)              local scripts, OSD)
   (~/.config/hwinfo64l/plugins — unprivileged)
```

### Daemon-less Graceful Fallback
If `hwinfo64ld` is not running, `hwinfo64l-cli` and `hwinfo64l-gui` run `hwinfo64l-core` in-process as the current user. All unprivileged probes (`hwmon`, CPU clocks/usage, `/proc/meminfo`, `zswap`, GPU sysfs, network, basic DMI) still work; root-only probes cleanly report `Unavailable (requires hwinfo64ld)` instead of failing.

## 2. Never-Fails Core Guarantees

1. **Zero-Panic Policy**: `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` in `hwinfo64l-protocol`, `hwinfo64l-core`, `hwinfo64l-probes`, and `hwinfo64ld`.
2. **Per-Probe Isolation & Circuit Breakers**:
   - Every probe runs on an isolated async task with a hard per-poll timeout.
   - Repeated failures trip a per-probe circuit breaker with exponential backoff (`5s -> 15s -> 60s -> 300s`), exposing diagnostic status in `ProbeHealth` while all other probes continue unaffected.
3. **Bounded Memory**:
   - Fixed-capacity ring buffer per sensor (default 3,600 samples = 1 hr @ 1 Hz).
   - O(1) incremental `min`, `max`, `avg`, and `sample_count`.
4. **Polling Tiers**:
   - **Static / Event-Driven**: SMBIOS/DMI, CPU topology, PCIe/USB buses, memory array layout.
   - **Fast (500–1000 ms)**: CPU clocks/usage/temps, RAPL power, GPU clocks/load, RAM/zswap, fans, network rates.
   - **Slow (5–30 s)**: SMART drive temperatures, wear, and health.

## 3. Extensibility Without Rebuilding

1. **Drop-in Probe Plugins** (`~/.config/hwinfo64l/plugins/` unprivileged; `/usr/lib/hwinfo64l/plugins/` system): Any executable in any language speaking NDJSON over stdin/stdout (see `docs/PROTOCOL.md`).
2. **Open Consumer API** (`/run/hwinfo64l/hwinfo64l.sock`): JSON-RPC 2.0 + live streaming subscriptions for any client.
3. **Declarative User Config** (`config.toml`): Sensor rename/hide, derived virtual sensors, threshold alerts, and CSV logging profiles.

## 4. Security & Privilege Hardening

- `hwinfo64ld` is **strictly read-only** — it never writes to hardware registers.
- Systemd hardening: `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, `RestrictAddressFamilies=AF_UNIX`, minimal capabilities (`CAP_SYS_RAWIO`, `CAP_SYS_ADMIN`, `CAP_DAC_READ_SEARCH`).
- User plugins in `~/.config/hwinfo64l/plugins/` **never** run as root.
