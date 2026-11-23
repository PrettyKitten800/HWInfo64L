#!/usr/bin/env python3
"""HWInfo64L Drop-In Plugin for Klipper / Kalico + Moonraker (Protocol v1).

Auto-discovers every heater (including Kalico MPC stats), temperature sensor,
fan, MCU, CAN bus node, TMC stepper driver, filament sensor, motion metric,
and print job statistic from Moonraker over HTTP — zero third-party Python
dependencies required.

Configuration via environment variables:
  MOONRAKER_URL          Base URL (default: http://127.0.0.1:7125)
  MOONRAKER_API_KEY      Optional X-Api-Key header for remote Moonraker hosts
  MOONRAKER_INTERVAL_MS  Poll interval in ms (default: 1000)
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

MOONRAKER_URL = os.environ.get("MOONRAKER_URL", "http://127.0.0.1:7125").rstrip("/")
MOONRAKER_API_KEY = os.environ.get("MOONRAKER_API_KEY", "").strip()
INTERVAL_MS = max(250, min(60_000, int(os.environ.get("MOONRAKER_INTERVAL_MS", "1000"))))

TMC_PREFIXES = ("tmc2209 ", "tmc2208 ", "tmc2130 ", "tmc2240 ", "tmc5160 ", "tmc2660 ")
FAN_PREFIXES = ("heater_fan ", "controller_fan ", "temperature_fan ", "fan_generic ")


def emit(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def sanitize(raw: str) -> str:
    s = re.sub(r"[^a-zA-Z0-9]+", "_", raw).strip("_").lower()
    return s or "unknown"


def pretty_name(raw: str) -> str:
    return " ".join(w.capitalize() if w.islower() else w for w in raw.replace("_", " ").split())


def http_get_json(path: str, timeout: float = 2.0) -> dict[str, Any]:
    req = urllib.request.Request(f"{MOONRAKER_URL}{path}")
    if MOONRAKER_API_KEY:
        req.add_header("X-Api-Key", MOONRAKER_API_KEY)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def make_sensor(
    sensor_id: str,
    group: str,
    name: str,
    kind: str,
    unit: str,
    *,
    min_exp: float | None = 0.0,
    max_exp: float | None = None,
    warn: float | None = None,
    crit: float | None = None,
) -> dict[str, Any]:
    desc: dict[str, Any] = {
        "id": sensor_id,
        "group": group,
        "name": name,
        "kind": kind,
        "unit": unit,
        "poll_tier": "fast",
        "source": "plugin:klipper_moonraker",
    }
    if min_exp is not None:
        desc["min_expected"] = min_exp
    if max_exp is not None:
        desc["max_expected"] = max_exp
    if warn is not None:
        desc["warn_threshold"] = warn
    if crit is not None:
        desc["crit_threshold"] = crit
    return desc


class MoonrakerPlugin:
    def __init__(self) -> None:
        self.query_objects: list[str] = []
        self.heaters: list[str] = []
        self.temp_sensors: list[str] = []
        self.fans: list[str] = []
        self.mcus: list[str] = []
        self.can_nodes: list[str] = []
        self.tmc_drivers: list[str] = []
        self.filament_sensors: list[str] = []
        self.has_mpc_extruder = False
        self.has_motion = False
        self.has_print_stats = False
        self.has_sdcard = False
        self.has_toolhead = False
        self.has_webhooks = False
        self.eddy_objects: list[str] = []
        self.firmware_app = "Klipper"

    def discover(self) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
        server_info = http_get_json("/server/info").get("result", {})
        obj_list: list[str] = http_get_json("/printer/objects/list").get("result", {}).get("objects", [])

        self.heaters = []
        self.temp_sensors = []
        self.fans = []
        self.mcus = []
        self.can_nodes = []
        self.tmc_drivers = []
        self.filament_sensors = []
        self.eddy_objects = []

        # Skip host temperature_sensor names that mirror temperature_host
        host_temp_names: set[str] = set()
        for o in obj_list:
            if o.startswith("temperature_host "):
                host_temp_names.add(o.split(" ", 1)[1])

        for o in obj_list:
            if o in ("extruder", "heater_bed") or o.startswith(("extruder", "heater_generic ")):
                if o.startswith("extruder") and " " in o:
                    continue
                self.heaters.append(o)
            elif o.startswith("temperature_sensor "):
                name = o.split(" ", 1)[1]
                if name not in host_temp_names:
                    self.temp_sensors.append(o)
            elif o == "fan" or o.startswith(FAN_PREFIXES):
                self.fans.append(o)
            elif o == "mcu" or o.startswith("mcu "):
                self.mcus.append(o)
            elif o.startswith("canbus_stats "):
                self.can_nodes.append(o)
            elif o.startswith(TMC_PREFIXES):
                self.tmc_drivers.append(o)
            elif o.startswith(("filament_switch_sensor ", "filament_motion_sensor ")):
                self.filament_sensors.append(o)
            elif o.startswith("probe_eddy_ng "):
                self.eddy_objects.append(o)

        self.has_motion = "motion_report" in obj_list
        self.has_print_stats = "print_stats" in obj_list
        self.has_sdcard = "virtual_sdcard" in obj_list
        self.has_toolhead = "toolhead" in obj_list
        self.has_webhooks = "webhooks" in obj_list

        self.query_objects = (
            self.heaters
            + self.temp_sensors
            + self.fans
            + self.mcus
            + self.can_nodes
            + self.tmc_drivers
            + self.filament_sensors
            + self.eddy_objects
            + [
                o
                for o, present in [
                    ("motion_report", self.has_motion),
                    ("print_stats", self.has_print_stats),
                    ("virtual_sdcard", self.has_sdcard),
                    ("toolhead", self.has_toolhead),
                    ("webhooks", self.has_webhooks),
                ]
                if present
            ]
        )

        # Fetch one initial snapshot so we can build accurate inventory & optional MPC/RPM/TMC sensors
        initial = self.fetch_status()
        if "mcu" in initial and isinstance(initial["mcu"], dict):
            self.firmware_app = str(initial["mcu"].get("app") or "Klipper")
        if "extruder" in initial and isinstance(initial["extruder"], dict):
            self.has_mpc_extruder = isinstance(initial["extruder"].get("control_stats"), dict)

        sensors: list[dict[str, Any]] = []
        inventory: list[dict[str, Any]] = []

        # Hardware Tree: Root 3D Printer node
        printer_props: dict[str, Any] = {
            "Firmware": self.firmware_app,
            "Moonraker Version": str(server_info.get("moonraker_version", "unknown")),
            "Moonraker URL": MOONRAKER_URL,
            "Klippy State": str(server_info.get("klippy_state", "unknown")),
        }
        if self.has_toolhead and isinstance(initial.get("toolhead"), dict):
            th = initial["toolhead"]
            amin = th.get("axis_minimum")
            amax = th.get("axis_maximum")
            if isinstance(amin, list) and isinstance(amax, list) and len(amin) >= 3 and len(amax) >= 3:
                printer_props["Build Volume (mm)"] = (
                    f"{amax[0] - amin[0]:.0f} x {amax[1] - amin[1]:.0f} x {amax[2] - amin[2]:.0f}"
                )
            if th.get("max_velocity") is not None:
                printer_props["Max Velocity (mm/s)"] = float(th["max_velocity"])
            if th.get("max_accel") is not None:
                printer_props["Max Accel (mm/s^2)"] = float(th["max_accel"])
        if self.has_print_stats and isinstance(initial.get("print_stats"), dict):
            ps = initial["print_stats"]
            if ps.get("filename"):
                printer_props["Last / Active File"] = str(ps["filename"])
            if ps.get("state"):
                printer_props["Job State"] = str(ps["state"])

        inventory.append(
            {
                "id": "klipper_printer",
                "parent_id": "system",
                "kind": "plugin",
                "label": f"3D Printer ({self.firmware_app} / Moonraker)",
                "properties": printer_props,
                "source": "plugin:klipper_moonraker",
            }
        )

        # 1. Heaters
        for h in self.heaters:
            key = sanitize(h)
            label = "Extruder" if h == "extruder" else ("Heater Bed" if h == "heater_bed" else pretty_name(h.split(" ", 1)[-1]))
            max_temp = 310.0 if "extruder" in h else 130.0
            sensors.append(make_sensor(f"plugin.klipper.heater.{key}_temp", "3D Printer: Heaters", f"{label} Temp", "temperature", "°C", max_exp=max_temp))
            sensors.append(make_sensor(f"plugin.klipper.heater.{key}_target", "3D Printer: Heaters", f"{label} Target", "temperature", "°C", max_exp=max_temp))
            sensors.append(make_sensor(f"plugin.klipper.heater.{key}_power", "3D Printer: Heaters", f"{label} Power", "utilization", "%", max_exp=100.0))

        if self.has_mpc_extruder:
            sensors.append(make_sensor("plugin.klipper.mpc.extruder_block_temp", "3D Printer: Heaters", "Extruder MPC Block Temp", "temperature", "°C", max_exp=310.0))
            sensors.append(make_sensor("plugin.klipper.mpc.extruder_ambient_temp", "3D Printer: Heaters", "Extruder MPC Ambient Temp", "temperature", "°C", max_exp=100.0))
            sensors.append(make_sensor("plugin.klipper.mpc.extruder_loss_ambient", "3D Printer: Heaters", "Extruder MPC Ambient Loss", "power", "W", min_exp=None))
            sensors.append(make_sensor("plugin.klipper.mpc.extruder_loss_filament", "3D Printer: Heaters", "Extruder MPC Filament Loss", "power", "W", min_exp=None))

        # 2. Temperature Sensors
        for ts_obj in self.temp_sensors:
            raw_name = ts_obj.split(" ", 1)[1]
            key = sanitize(raw_name)
            sensors.append(
                make_sensor(
                    f"plugin.klipper.temp.{key}",
                    "3D Printer: Temperatures",
                    pretty_name(raw_name),
                    "temperature",
                    "°C",
                    max_exp=100.0,
                    warn=75.0,
                    crit=85.0,
                )
            )

        # 3. Fans
        for f_obj in self.fans:
            key = sanitize(f_obj)
            label = "Part Cooling Fan" if f_obj == "fan" else pretty_name(f_obj.split(" ", 1)[1])
            sensors.append(make_sensor(f"plugin.klipper.fan.{key}_speed", "3D Printer: Fans", f"{label} Speed", "utilization", "%", max_exp=100.0))
            f_state = initial.get(f_obj, {})
            if isinstance(f_state, dict) and f_state.get("rpm") is not None:
                sensors.append(make_sensor(f"plugin.klipper.fan.{key}_rpm", "3D Printer: Fans", f"{label} RPM", "fan_rpm", "RPM"))

        # 4. MCUs (Sensors + Hardware Tree Inventory)
        for mcu_obj in self.mcus:
            short = "main" if mcu_obj == "mcu" else mcu_obj.split(" ", 1)[1]
            key = sanitize(short)
            label = "MCU (Main)" if mcu_obj == "mcu" else f"MCU ({pretty_name(short)})"
            m_data = initial.get(mcu_obj, {})
            consts = m_data.get("mcu_constants", {}) if isinstance(m_data, dict) else {}
            chip = str(consts.get("MCU", "unknown"))

            mcu_props: dict[str, Any] = {
                "Chip": chip,
                "App": str(m_data.get("app", self.firmware_app)) if isinstance(m_data, dict) else self.firmware_app,
                "Firmware Version": str(m_data.get("mcu_version", "unknown")) if isinstance(m_data, dict) else "unknown",
            }
            if isinstance(m_data, dict) and m_data.get("mcu_build_versions"):
                mcu_props["Compiler"] = str(m_data["mcu_build_versions"])
            if consts.get("CLOCK_FREQ"):
                mcu_props["Nominal Clock (MHz)"] = round(float(consts["CLOCK_FREQ"]) / 1e6, 2)
            if consts.get("CANBUS_BRIDGE"):
                mcu_props["CAN Bus Bridge"] = True

            inventory.append(
                {
                    "id": f"klipper_mcu_{key}",
                    "parent_id": "klipper_printer",
                    "kind": "plugin",
                    "label": f"{label} — {chip}",
                    "properties": mcu_props,
                    "source": "plugin:klipper_moonraker",
                }
            )

            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_awake", "3D Printer: MCUs", f"{label} Awake", "utilization", "%", max_exp=100.0, warn=60.0, crit=85.0))
            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_task_us", "3D Printer: MCUs", f"{label} Task Avg", "custom", "µs"))
            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_clock_mhz", "3D Printer: MCUs", f"{label} Actual Clock", "clock", "MHz"))
            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_srtt_ms", "3D Printer: MCUs", f"{label} Round-Trip (SRTT)", "custom", "ms"))
            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_retransmit", "3D Printer: MCUs", f"{label} Retransmit Bytes", "counter", "B", warn=1.0))
            sensors.append(make_sensor(f"plugin.klipper.mcu.{key}_invalid", "3D Printer: MCUs", f"{label} Invalid Bytes", "counter", "B", warn=1.0))

        # 5. CAN Bus Stats
        for can_obj in self.can_nodes:
            node = can_obj.split(" ", 1)[1]
            key = sanitize(node)
            label = f"CAN ({pretty_name(node)})"
            sensors.append(make_sensor(f"plugin.klipper.can.{key}_rx_err", "3D Printer: CAN Bus", f"{label} RX Errors", "counter", "", warn=1.0))
            sensors.append(make_sensor(f"plugin.klipper.can.{key}_tx_err", "3D Printer: CAN Bus", f"{label} TX Errors", "counter", "", warn=1.0))
            sensors.append(make_sensor(f"plugin.klipper.can.{key}_tx_retries", "3D Printer: CAN Bus", f"{label} TX Retries", "counter", ""))
            sensors.append(make_sensor(f"plugin.klipper.can.{key}_active", "3D Printer: CAN Bus", f"{label} Bus Active", "boolean_flag", "", max_exp=1.0))

        # 6. TMC Stepper Drivers
        for tmc_obj in self.tmc_drivers:
            drv_type, stepper = tmc_obj.split(" ", 1)
            key = sanitize(stepper)
            label = f"{pretty_name(stepper)} ({drv_type.upper()})"
            sensors.append(make_sensor(f"plugin.klipper.tmc.{key}_run_a", "3D Printer: Steppers (TMC)", f"{label} Run Current", "current", "A", max_exp=2.5))
            t_state = initial.get(tmc_obj, {})
            if isinstance(t_state, dict) and t_state.get("temperature") is not None:
                sensors.append(make_sensor(f"plugin.klipper.tmc.{key}_temp", "3D Printer: Steppers (TMC)", f"{label} Driver Temp", "temperature", "°C", max_exp=130.0, warn=100.0, crit=120.0))

        # 7. Eddy NG Probe Inventory
        for eddy_obj in self.eddy_objects:
            e_data = initial.get(eddy_obj, {})
            if isinstance(e_data, dict):
                e_props: dict[str, Any] = {
                    "Active Plate": str(e_data.get("active_plate", "default")),
                    "Tap Profile": str(e_data.get("active_tap_profile", "default")),
                }
                if e_data.get("tap_adjust_z") is not None:
                    e_props["Tap Adjust Z (mm)"] = float(e_data["tap_adjust_z"])
                if e_data.get("last_tap_z") is not None:
                    e_props["Last Tap Z (mm)"] = round(float(e_data["last_tap_z"]), 4)
                inventory.append(
                    {
                        "id": f"klipper_{sanitize(eddy_obj)}",
                        "parent_id": "klipper_printer",
                        "kind": "plugin",
                        "label": f"Eddy NG Probe ({eddy_obj.split(' ', 1)[1]})",
                        "properties": e_props,
                        "source": "plugin:klipper_moonraker",
                    }
                )

        # 8. Motion, Print Job & Filament
        if self.has_webhooks:
            sensors.append(make_sensor("plugin.klipper.state.ready", "3D Printer: Motion & Print", "Printer Ready", "boolean_flag", "", max_exp=1.0))
        if self.has_sdcard:
            sensors.append(make_sensor("plugin.klipper.job.progress", "3D Printer: Motion & Print", "Print Progress", "utilization", "%", max_exp=100.0))
        if self.has_print_stats:
            sensors.append(make_sensor("plugin.klipper.job.filament_m", "3D Printer: Motion & Print", "Filament Used", "custom", "m"))
            sensors.append(make_sensor("plugin.klipper.job.duration_min", "3D Printer: Motion & Print", "Print Duration", "custom", "min"))
        if self.has_motion:
            sensors.append(make_sensor("plugin.klipper.motion.velocity", "3D Printer: Motion & Print", "Live Toolhead Velocity", "custom", "mm/s"))
            sensors.append(make_sensor("plugin.klipper.motion.extruder_vel", "3D Printer: Motion & Print", "Live Extruder Velocity", "custom", "mm/s", min_exp=None))
            sensors.append(make_sensor("plugin.klipper.motion.z_height", "3D Printer: Motion & Print", "Current Z Height", "custom", "mm"))
        if self.has_toolhead:
            sensors.append(make_sensor("plugin.klipper.motion.stalls", "3D Printer: Motion & Print", "Toolhead Stalls", "counter", "", warn=1.0))
        for fs_obj in self.filament_sensors:
            raw_name = fs_obj.split(" ", 1)[1]
            key = sanitize(raw_name)
            sensors.append(make_sensor(f"plugin.klipper.filament.{key}", "3D Printer: Motion & Print", f"Filament ({pretty_name(raw_name)})", "boolean_flag", "", max_exp=1.0))

        return sensors, inventory

    def fetch_status(self) -> dict[str, Any]:
        if not self.query_objects:
            return {}
        q = "&".join(urllib.parse.quote(o) for o in self.query_objects)
        return http_get_json(f"/printer/objects/query?{q}").get("result", {}).get("status", {})

    def poll_values(self) -> dict[str, float]:
        status = self.fetch_status()
        vals: dict[str, float] = {}

        for h in self.heaters:
            d = status.get(h)
            if not isinstance(d, dict):
                continue
            key = sanitize(h)
            if d.get("temperature") is not None:
                vals[f"plugin.klipper.heater.{key}_temp"] = float(d["temperature"])
            if d.get("target") is not None:
                vals[f"plugin.klipper.heater.{key}_target"] = float(d["target"])
            if d.get("power") is not None:
                vals[f"plugin.klipper.heater.{key}_power"] = float(d["power"]) * 100.0

        if self.has_mpc_extruder:
            cs = (status.get("extruder") or {}).get("control_stats")
            if isinstance(cs, dict):
                if cs.get("temp_block") is not None:
                    vals["plugin.klipper.mpc.extruder_block_temp"] = float(cs["temp_block"])
                if cs.get("temp_ambient") is not None:
                    vals["plugin.klipper.mpc.extruder_ambient_temp"] = float(cs["temp_ambient"])
                if cs.get("loss_ambient") is not None:
                    vals["plugin.klipper.mpc.extruder_loss_ambient"] = float(cs["loss_ambient"])
                if cs.get("loss_filament") is not None:
                    vals["plugin.klipper.mpc.extruder_loss_filament"] = float(cs["loss_filament"])

        for ts_obj in self.temp_sensors:
            d = status.get(ts_obj)
            if isinstance(d, dict) and d.get("temperature") is not None:
                key = sanitize(ts_obj.split(" ", 1)[1])
                vals[f"plugin.klipper.temp.{key}"] = float(d["temperature"])

        for f_obj in self.fans:
            d = status.get(f_obj)
            if not isinstance(d, dict):
                continue
            key = sanitize(f_obj)
            if d.get("speed") is not None:
                vals[f"plugin.klipper.fan.{key}_speed"] = float(d["speed"]) * 100.0
            if d.get("rpm") is not None:
                vals[f"plugin.klipper.fan.{key}_rpm"] = float(d["rpm"])

        for mcu_obj in self.mcus:
            d = status.get(mcu_obj)
            if not isinstance(d, dict):
                continue
            ls = d.get("last_stats")
            if not isinstance(ls, dict):
                continue
            short = "main" if mcu_obj == "mcu" else mcu_obj.split(" ", 1)[1]
            key = sanitize(short)
            if ls.get("mcu_awake") is not None:
                vals[f"plugin.klipper.mcu.{key}_awake"] = float(ls["mcu_awake"]) * 100.0
            if ls.get("mcu_task_avg") is not None:
                vals[f"plugin.klipper.mcu.{key}_task_us"] = float(ls["mcu_task_avg"]) * 1_000_000.0
            if ls.get("freq") is not None:
                vals[f"plugin.klipper.mcu.{key}_clock_mhz"] = float(ls["freq"]) / 1_000_000.0
            if ls.get("srtt") is not None:
                vals[f"plugin.klipper.mcu.{key}_srtt_ms"] = float(ls["srtt"]) * 1000.0
            if ls.get("bytes_retransmit") is not None:
                vals[f"plugin.klipper.mcu.{key}_retransmit"] = float(ls["bytes_retransmit"])
            if ls.get("bytes_invalid") is not None:
                vals[f"plugin.klipper.mcu.{key}_invalid"] = float(ls["bytes_invalid"])

        for can_obj in self.can_nodes:
            d = status.get(can_obj)
            if not isinstance(d, dict):
                continue
            key = sanitize(can_obj.split(" ", 1)[1])
            if d.get("rx_error") is not None:
                vals[f"plugin.klipper.can.{key}_rx_err"] = float(d["rx_error"])
            if d.get("tx_error") is not None:
                vals[f"plugin.klipper.can.{key}_tx_err"] = float(d["tx_error"])
            if d.get("tx_retries") is not None:
                vals[f"plugin.klipper.can.{key}_tx_retries"] = float(d["tx_retries"])
            if d.get("bus_state") is not None:
                vals[f"plugin.klipper.can.{key}_active"] = 1.0 if str(d["bus_state"]).lower() == "active" else 0.0

        for tmc_obj in self.tmc_drivers:
            d = status.get(tmc_obj)
            if not isinstance(d, dict):
                continue
            key = sanitize(tmc_obj.split(" ", 1)[1])
            if d.get("run_current") is not None:
                vals[f"plugin.klipper.tmc.{key}_run_a"] = float(d["run_current"])
            if d.get("temperature") is not None:
                vals[f"plugin.klipper.tmc.{key}_temp"] = float(d["temperature"])

        if self.has_webhooks:
            wh = status.get("webhooks")
            if isinstance(wh, dict):
                vals["plugin.klipper.state.ready"] = 1.0 if wh.get("state") == "ready" else 0.0

        if self.has_sdcard:
            sd = status.get("virtual_sdcard")
            if isinstance(sd, dict) and sd.get("progress") is not None:
                vals["plugin.klipper.job.progress"] = float(sd["progress"]) * 100.0

        if self.has_print_stats:
            ps = status.get("print_stats")
            if isinstance(ps, dict):
                if ps.get("filament_used") is not None:
                    vals["plugin.klipper.job.filament_m"] = float(ps["filament_used"]) / 1000.0
                if ps.get("print_duration") is not None:
                    vals["plugin.klipper.job.duration_min"] = float(ps["print_duration"]) / 60.0

        if self.has_motion:
            mr = status.get("motion_report")
            if isinstance(mr, dict):
                if mr.get("live_velocity") is not None:
                    vals["plugin.klipper.motion.velocity"] = float(mr["live_velocity"])
                if mr.get("live_extruder_velocity") is not None:
                    vals["plugin.klipper.motion.extruder_vel"] = float(mr["live_extruder_velocity"])
                pos = mr.get("live_position")
                if isinstance(pos, list) and len(pos) >= 3:
                    vals["plugin.klipper.motion.z_height"] = float(pos[2])

        if self.has_toolhead:
            th = status.get("toolhead")
            if isinstance(th, dict) and th.get("stalls") is not None:
                vals["plugin.klipper.motion.stalls"] = float(th["stalls"])

        for fs_obj in self.filament_sensors:
            d = status.get(fs_obj)
            if isinstance(d, dict) and d.get("filament_detected") is not None:
                key = sanitize(fs_obj.split(" ", 1)[1])
                vals[f"plugin.klipper.filament.{key}"] = 1.0 if d["filament_detected"] else 0.0

        return vals


def main() -> None:
    plugin = MoonrakerPlugin()
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as exc:
            emit({"type": "error", "message": f"invalid host JSON: {exc}"})
            continue

        kind = msg.get("type")
        if kind == "hello":
            try:
                sensors, inventory = plugin.discover()
                emit(
                    {
                        "type": "manifest",
                        "id": "klipper_moonraker",
                        "name": f"Klipper / {plugin.firmware_app} (Moonraker)",
                        "version": "0.1.0",
                        "interval_ms": INTERVAL_MS,
                        "sensors": sensors,
                        "inventory": inventory,
                    }
                )
            except Exception as exc:
                emit({"type": "error", "message": f"Moonraker discovery failed ({MOONRAKER_URL}): {exc}"})
                return
        elif kind == "poll":
            try:
                values = plugin.poll_values()
                emit(
                    {
                        "type": "readings",
                        "timestamp_ms": int(time.time() * 1000),
                        "values": values,
                    }
                )
            except Exception as exc:
                emit({"type": "error", "message": f"Moonraker poll error: {exc}"})
        elif kind == "shutdown":
            break


if __name__ == "__main__":
    main()
