#!/usr/bin/env python3
"""Example drop-in HWInfo64L NDJSON plugin (Protocol v1).

Copy or symlink this executable into ~/.config/hwinfo64l/plugins/ to expose
Linux 1m/5m/15m load averages without recompiling HWInfo64L.
"""

import json
import os
import sys
import time


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


MANIFEST = {
    "type": "manifest",
    "id": "loadavg",
    "name": "System Load Average (Example Plugin)",
    "version": "0.1.0",
    "interval_ms": 1000,
    "sensors": [
        {
            "id": "plugin.loadavg.system.ratio.1m",
            "group": "System Load (Plugin)",
            "name": "Load Average (1m)",
            "kind": "ratio",
            "unit": "",
            "poll_tier": "fast",
            "source": "plugin:loadavg",
        },
        {
            "id": "plugin.loadavg.system.ratio.5m",
            "group": "System Load (Plugin)",
            "name": "Load Average (5m)",
            "kind": "ratio",
            "unit": "",
            "poll_tier": "fast",
            "source": "plugin:loadavg",
        },
    ],
    "inventory": [],
}


def main() -> None:
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        msg = json.loads(line)
        kind = msg.get("type")
        if kind == "hello":
            emit(MANIFEST)
        elif kind == "poll":
            l1, l5, _ = os.getloadavg()
            emit(
                {
                    "type": "readings",
                    "timestamp_ms": int(time.time() * 1000),
                    "values": {
                        "plugin.loadavg.system.ratio.1m": round(l1, 2),
                        "plugin.loadavg.system.ratio.5m": round(l5, 2),
                    },
                }
            )
        elif kind == "shutdown":
            break


if __name__ == "__main__":
    main()
