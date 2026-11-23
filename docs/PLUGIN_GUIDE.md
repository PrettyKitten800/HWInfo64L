# HWInfo64L Drop-In Plugin Guide

HWInfo64L supports drop-in hardware/telemetry plugins written in **any language** (Python, Bash, Rust, Go, C, Node.js) with **zero core recompilation**.

## Where Plugins Live

| Directory | Runs Under | Best For |
| :--- | :--- | :--- |
| `~/.config/hwinfo64l/plugins/` | Unprivileged user (in `hwinfo64l-gui` / `hwinfo64l --local`) | Network APIs (Moonraker/Klipper, Home Assistant, UPS, MQTT), custom user scripts |
| `/usr/local/share/hwinfo64l/plugins/` | Unprivileged `hwinfo64l-web` (`--plugin-dir`) | Shared unprivileged plugins visible on the LAN Web UI + Prometheus `/metrics` |
| `/usr/lib/hwinfo64l/plugins/` | `hwinfo64ld` (strictly `AF_UNIX`, read-only) | Local-only privileged system hardware probes |

## Included Flagship Plugin: Klipper / Kalico (Moonraker)

`plugins/klipper-moonraker/klipper_moonraker.py` auto-discovers every heater (including Kalico MPC telemetry), temperature sensor, fan, MCU (with Hardware Tree chip/firmware inventory), CAN bus error/retry counters, TMC stepper driver currents/temps, Eddy NG probe info, motion velocity, and print job metrics.

### Install for Your User (GTK4 GUI)
```bash
mkdir -p ~/.config/hwinfo64l/plugins
cp plugins/klipper-moonraker/klipper_moonraker.py ~/.config/hwinfo64l/plugins/
chmod +x ~/.config/hwinfo64l/plugins/klipper_moonraker.py
```

### Optional Environment Variables
- `MOONRAKER_URL` — defaults to `http://127.0.0.1:7125`
- `MOONRAKER_API_KEY` — optional API key for remote Moonraker instances
- `MOONRAKER_INTERVAL_MS` — polling cadence in milliseconds (default `1000`)

## Writing Your Own Plugin (30-Second Protocol)

1. Read newline-delimited JSON commands from `stdin`:
   - `{"type":"hello","protocol_version":1}` → reply with `{"type":"manifest", ...}` declaring `id`, `name`, `version`, `interval_ms`, `sensors[]`, and optional `inventory[]`.
   - `{"type":"poll"}` → reply with `{"type":"readings","timestamp_ms":...,"values":{"sensor.id": 42.0}}`.
   - `{"type":"shutdown"}` → exit cleanly.
2. See `examples/plugins/loadavg_plugin.py` for a minimal 35-line template and `docs/PROTOCOL.md` for full schema details.
