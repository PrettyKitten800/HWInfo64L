# HWInfo64L Protocol Specification (v1)

This document defines the stable `v1` contracts shared across `hwinfo64ld`, `hwinfo64l-cli`, `hwinfo64l-gui`, external consumers, and drop-in probe plugins.

> **Compatibility rule:** Within major version `1`, changes are **strictly additive**. Existing fields, method names, and sensor ID formats are never removed or given breaking semantics.

---

## 1. Sensor ID Format

Deterministic, dot-delimited lowercase ASCII identifiers:

```text
<provider>.<device_key>.<category>.<metric>
```

Examples:
- `coretemp.pkg0.temp.package`
- `coretemp.pkg0.temp.core0`
- `rapl.pkg0.power.package`
- `rapl.pkg0.power.pl1_limit`
- `cpufreq.cpu0.clock.current`
- `cpustat.system.util.total`
- `memory.system.size.used`
- `zswap.system.size.compressed`
- `smart.sda.temp.drive`
- `net.enp2s0.rate.rx`
- `plugin.klipper.temp.extruder`

---

## 2. Daemon Wire Protocol (Unix Domain Socket)

- **Default socket path:** `/run/hwinfo64l/hwinfo64l.sock` (override via `HWINFO64L_SOCKET`)
- **Framing:** Newline-delimited JSON (`NDJSON`), one UTF-8 JSON object per `\n`-terminated line.
- **Envelope:** JSON-RPC 2.0 request/response + server-pushed notifications for subscriptions.

### Methods (`v1`)

| Method | Params | Result |
| :--- | :--- | :--- |
| `v1.hello` | `{ "client": "hwinfo64l-gui", "protocol_version": 1 }` | `{ "daemon_version": "...", "protocol_version": 1, "hostname": "...", "uptime_secs": 123 }` |
| `v1.get_summary` | `{}` | `SystemSummary` (CPU, Motherboard/BIOS, Memory, GPUs, Drives, OS) |
| `v1.get_inventory` | `{}` | `Vec<InventoryNode>` (full hardware tree) |
| `v1.list_sensors` | `{}` | `{ "descriptors": Vec<SensorDescriptor>, "readings": Vec<SensorReading>, "probes": Vec<ProbeHealth> }` |
| `v1.get_history` | `{ "id": "coretemp.pkg0.temp.package", "limit": 300 }` | `{ "id": "...", "samples": Vec<HistorySample> }` |
| `v1.reset_stats` | `{ "ids": null \| Vec<String> }` | `{ "reset_count": 42 }` |
| `v1.subscribe` | `{ "ids": null \| Vec<String>, "interval_ms": 1000 }` | `{ "subscribed": true }` followed by periodic `v1.sensor_batch` notifications |

### Server Notification (`v1.sensor_batch`)

```json
{
  "jsonrpc": "2.0",
  "method": "v1.sensor_batch",
  "params": {
    "timestamp_ms": 1795380000000,
    "readings": [
      {
        "id": "coretemp.pkg0.temp.package",
        "timestamp_ms": 1795380000000,
        "value": 67.0,
        "status": "ok",
        "stats": { "min": 61.0, "max": 74.0, "avg": 66.4, "samples": 120 }
      }
    ],
    "probes": []
  }
}
```

---

## 3. Drop-In Probe Plugin Protocol (NDJSON over stdin/stdout)

Plugins are standalone executables placed in:
- `~/.config/hwinfo64l/plugins/<plugin-name>/` (unprivileged user session)
- `/usr/lib/hwinfo64l/plugins/<plugin-name>/` (optional system-wide)

### Handshake & Lifecycle

1. **Host → Plugin (`stdin`):**
   ```json
   {"type":"hello","protocol_version":1}
   ```
2. **Plugin → Host (`stdout`):**
   ```json
   {
     "type": "manifest",
     "id": "klipper",
     "name": "Klipper / Moonraker",
     "version": "0.1.0",
     "interval_ms": 1000,
     "sensors": [
       {
         "id": "plugin.klipper.temp.extruder",
         "group": "3D Printer (Klipper)",
         "name": "Extruder",
         "kind": "temperature",
         "unit": "°C",
         "poll_tier": "fast",
         "source": "plugin:klipper"
       }
     ],
     "inventory": []
   }
   ```
3. **Readings (Push or Poll):**
   - Host may send `{"type":"poll"}` on the plugin's `interval_ms`, or plugin may push autonomously:
   ```json
   {"type":"readings","timestamp_ms":1795380000000,"values":{"plugin.klipper.temp.extruder":210.4}}
   ```
4. **Shutdown:** Host sends `{"type":"shutdown"}` and closes `stdin`, waiting up to 2 s before terminating.
