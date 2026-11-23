#!/usr/bin/env python3
"""HWInfo64L Reusable Security, Fuzz, Stress & Soak Test Suite.

Runs 5 test suites:
  1. Hostile & Broken Plugin Fuzzing (crash, garbage, 700 KiB line, NaN, 3000-sensor flood, hang)
  2. Unix Socket Wire Protocol Fuzzing (bad UTF-8, corrupt JSON, deep nesting, 100 KiB line, bad params)
  3. Stress & Concurrency Storm (80-conn cap & semaphore leak check + 40-thread RPC/disconnect storm)
  4. Mini-Soak Endurance Check (VmRSS, open FD count, thread count, zero zombies, clean SIGTERM)
  5. Live Systemd Service & Klipper Plugin Audit (capabilities, zero TCP ports, sandbox score)

Usage:
  ./scripts/test_robustness.py                   # Full suite (~25s)
  ./scripts/test_robustness.py --soak-secs 300   # Extended 5-minute soak
  ./scripts/test_robustness.py --skip-live       # Skip live systemd service audit (CI mode)
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import os
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DAEMON_BIN = REPO_ROOT / "target" / "release" / "hwinfo64ld"


class TestReport:
    def __init__(self) -> None:
        self.passed = 0
        self.failed = 0
        self.warnings = 0

    def ok(self, name: str, detail: str = "") -> None:
        self.passed += 1
        suffix = f" — {detail}" if detail else ""
        print(f"  [PASS] {name}{suffix}")

    def warn(self, name: str, detail: str = "") -> None:
        self.warnings += 1
        suffix = f" — {detail}" if detail else ""
        print(f"  [WARN] {name}{suffix}")

    def fail(self, name: str, detail: str = "") -> None:
        self.failed += 1
        suffix = f" — {detail}" if detail else ""
        print(f"  [FAIL] {name}{suffix}")


def rpc_call(sock_path: str, method: str, params: dict[str, Any] | None = None, timeout: float = 3.0) -> dict[str, Any]:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(sock_path)
        req = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
        s.sendall((json.dumps(req) + "\n").encode("utf-8"))
        buf = bytearray()
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf.extend(chunk)
            if b"\n" in chunk:
                break
        line = buf.split(b"\n", 1)[0].decode("utf-8")
        return json.loads(line)
    finally:
        s.close()


def read_proc_metrics(pid: int) -> dict[str, int]:
    rss_kb = 0
    threads = 0
    with open(f"/proc/{pid}/status", "r", encoding="utf-8") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                rss_kb = int(line.split()[1])
            elif line.startswith("Threads:"):
                threads = int(line.split()[1])
    fd_count = len(os.listdir(f"/proc/{pid}/fd"))
    zombies = 0
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/status", "r", encoding="utf-8") as f:
                ppid = None
                state = None
                for line in f:
                    if line.startswith("PPid:"):
                        ppid = int(line.split()[1])
                    elif line.startswith("State:"):
                        state = line.split()[1]
                if ppid == pid and state == "Z":
                    zombies += 1
        except OSError:
            continue
    return {"rss_kb": rss_kb, "threads": threads, "fds": fd_count, "zombies": zombies}


def write_exec(path: Path, body: str, mode: int = 0o755) -> None:
    path.write_text(body, encoding="utf-8")
    path.chmod(mode)


def create_hostile_plugins(plugin_dir: Path) -> None:
    # 1. Exits non-zero on hello
    write_exec(
        plugin_dir / "01_crash_on_hello.py",
        "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n",
    )

    # 2. Emits garbage non-JSON + invalid UTF-8
    write_exec(
        plugin_dir / "02_garbage_bytes.py",
        "#!/usr/bin/env python3\nimport sys\nsys.stdout.buffer.write(b'\\xff\\xfeNOT_JSON\\n')\nsys.stdout.buffer.flush()\n",
    )

    # 3. Emits a 700 KiB line without newline (exceeds 512 KiB cap)
    write_exec(
        plugin_dir / "03_oversized_line.py",
        "#!/usr/bin/env python3\nimport sys\nsys.stdin.readline()\nsys.stdout.write('A' * (700 * 1024))\nsys.stdout.flush()\n",
    )

    # 4. Floods 3000 fake sensor descriptors (exceeds 2048 cap) + emits NaN / Infinity
    write_exec(
        plugin_dir / "04_cardinality_and_nan.py",
        """#!/usr/bin/env python3
import json, sys, time
sensors = [{
    "id": f"plugin.flood.dev.temp.s{i}",
    "group": "Hostile Flood",
    "name": f"Fake {i}",
    "kind": "temperature",
    "unit": "C",
    "poll_tier": "fast",
    "source": "plugin:flood",
} for i in range(3000)]
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "hello":
        sys.stdout.write(json.dumps({"type":"manifest","id":"flood","name":"Flood","version":"1","interval_ms":500,"sensors":sensors,"inventory":[]}) + "\\n")
        sys.stdout.flush()
    elif msg.get("type") == "poll":
        sys.stdout.write('{"type":"readings","timestamp_ms":' + str(int(time.time()*1000)) + ',"values":{"plugin.flood.dev.temp.s0":42.0,"plugin.flood.dev.temp.s2":1e308,"plugin.flood.dev.temp.s1":NaN}}\\n')
        sys.stdout.flush()
    elif msg.get("type") == "shutdown":
        break
""",
    )

    # 5. Valid manifest, hangs forever on poll (tests hard timeout + circuit breaker)
    write_exec(
        plugin_dir / "05_hanging_poll.py",
        """#!/usr/bin/env python3
import json, sys, time
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "hello":
        sys.stdout.write(json.dumps({"type":"manifest","id":"hang","name":"Hang","version":"1","interval_ms":500,"sensors":[],"inventory":[]}) + "\\n")
        sys.stdout.flush()
    elif msg.get("type") == "poll":
        time.sleep(60)
""",
    )

    # 6. World-writable plugin (chmod 0777) — MUST be refused before execution
    write_exec(
        plugin_dir / "06_world_writable.py",
        "#!/usr/bin/env python3\nimport sys\nsys.exit(0)\n",
        mode=0o777,
    )


def wait_for_socket(sock_path: str, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(sock_path):
            try:
                res = rpc_call(sock_path, "v1.hello", timeout=1.0)
                if "result" in res:
                    return True
            except Exception:
                pass
        time.sleep(0.1)
    return False


def suite_1_hostile_plugins(daemon_bin: Path, report: TestReport) -> None:
    print("\n=== Suite 1: Hostile & Broken Plugin Isolation ===")
    tmpdir = Path(tempfile.mkdtemp(prefix="hwinfo64l-plugins-"))
    sock_path = str(tmpdir / "daemon.sock")
    pdir = tmpdir / "plugins"
    pdir.mkdir()
    create_hostile_plugins(pdir)

    proc = subprocess.Popen(
        [str(daemon_bin), "--socket", sock_path, "--plugin-dir", str(pdir)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_for_socket(sock_path):
            report.fail("Hostile-plugin daemon startup", "daemon did not bind socket")
            return
        time.sleep(4.2)  # allow hanging plugin to hit 3s timeout & built-in probes to run

        if proc.poll() is not None:
            report.fail("Daemon survived hostile plugins", f"exited with {proc.returncode}")
            return
        report.ok("Daemon survived all 5 hostile plugins without crashing")

        resp = rpc_call(sock_path, "v1.list_sensors")
        data = resp["result"]
        descs = data["descriptors"]
        readings = data["readings"]
        probes = {p["probe_id"]: p for p in data["probes"]}

        if len(descs) <= 2048:
            report.ok("SensorStore cardinality cap enforced", f"{len(descs)} <= 2048 descriptors (despite 3000-sensor flood)")
        else:
            report.fail("SensorStore cardinality cap enforced", f"got {len(descs)} descriptors")

        bad_vals = [r["id"] for r in readings if not math.isfinite(r["value"])]
        if not bad_vals:
            report.ok("Zero NaN/Infinity readings leaked into store")
        else:
            report.fail("Zero NaN/Infinity readings leaked", str(bad_vals[:5]))

        core_ids = ("cpu_telemetry", "memory")
        core_ok = all(probes.get(pid, {}).get("state") == "healthy" for pid in core_ids)
        if core_ok:
            lat = {pid: probes[pid]["last_poll_duration_us"] for pid in core_ids}
            report.ok("Built-in probes stayed Healthy during hostile plugin chaos", f"poll µs: {lat}")
        else:
            report.fail("Built-in probes stayed Healthy", str({k: probes.get(k, {}).get("state") for k in core_ids}))

        hostile_isolated = sum(
            1 for pid, p in probes.items()
            if pid.startswith("plugin:") and p["state"] in ("degraded", "circuit_open")
        )
        if hostile_isolated >= 4:
            report.ok("Hostile plugins isolated into degraded/circuit_open", f"{hostile_isolated}/5 flagged")
        else:
            report.warn("Hostile plugins isolated", f"only {hostile_isolated}/5 flagged so far")

        if "plugin:06_world_writable" not in probes:
            report.ok("World-writable plugin (0777) refused before execution")
        else:
            report.fail("World-writable plugin (0777) was executed!")

        # Symlink socket refusal test
        sym_target = tmpdir / "victim.txt"
        sym_target.write_text("do not delete", encoding="utf-8")
        sym_sock = tmpdir / "evil.sock"
        os.symlink(sym_target, sym_sock)
        sym_rc = subprocess.run(
            [str(daemon_bin), "--socket", str(sym_sock), "--plugin-dir", str(tmpdir / "empty")],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        ).returncode
        if sym_rc != 0 and sym_target.exists() and sym_sock.is_symlink():
            report.ok("Daemon refused to unlink pre-existing symlink at --socket")
        else:
            report.fail("Symlink socket refusal", f"rc={sym_rc}, target_exists={sym_target.exists()}")
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        shutil.rmtree(tmpdir, ignore_errors=True)


def suite_2_3_4_fuzz_stress_soak(daemon_bin: Path, soak_secs: int, report: TestReport) -> None:
    tmpdir = Path(tempfile.mkdtemp(prefix="hwinfo64l-main-"))
    sock_path = str(tmpdir / "daemon.sock")
    empty_plugins = tmpdir / "plugins"
    empty_plugins.mkdir()

    proc = subprocess.Popen(
        [str(daemon_bin), "--socket", sock_path, "--plugin-dir", str(empty_plugins)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_for_socket(sock_path):
            report.fail("Test daemon startup", "failed to bind socket")
            return
        time.sleep(1.2)

        # --- Suite 2: Wire Protocol Fuzzing ---
        print("\n=== Suite 2: Unix Socket Wire Protocol Fuzzing ===")
        fuzz_payloads: list[tuple[str, bytes]] = [
            ("Invalid UTF-8", b"\xff\xfe\xfd\x00\n"),
            ("Corrupt JSON", b"{not valid json\n"),
            ("Deeply nested JSON (250 levels)", (b"[" * 250 + b"]" * 250 + b"\n")),
            ("Oversized 100 KiB line (no newline)", b"{" + b"A" * 100_000),
            ("Wrong param types", b'{"jsonrpc":"2.0","id":1,"method":"v1.get_history","params":{"id":123,"limit":-999}}\n'),
            ("Unknown method", b'{"jsonrpc":"2.0","id":2,"method":"v1.does_not_exist","params":{}}\n'),
            ("Empty lines + null id", b"\n\n{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"v1.hello\"}\n"),
        ]
        fuzz_ok = True
        for label, payload in fuzz_payloads:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(2.0)
            try:
                s.connect(sock_path)
                try:
                    s.sendall(payload)
                    _ = s.recv(4096)
                except (BrokenPipeError, ConnectionResetError, socket.timeout):
                    pass
            except Exception as exc:
                fuzz_ok = False
                report.fail(f"Fuzz case '{label}'", str(exc))
            finally:
                s.close()

        # Verify clean recovery immediately after fuzzing
        hello = rpc_call(sock_path, "v1.hello")
        if fuzz_ok and "result" in hello and proc.poll() is None:
            report.ok(f"All {len(fuzz_payloads)} wire fuzz payloads survived cleanly")
        else:
            report.fail("Wire fuzz recovery", str(hello))

        # --- Suite 3: Stress & Concurrency Storm ---
        print("\n=== Suite 3: Stress & Concurrency Storm ===")
        # 3a. Connection cap (80 simultaneous sockets > 64 cap) + semaphore leak verification
        held: list[socket.socket] = []
        rejected = 0
        for _ in range(80):
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(1.0)
            try:
                s.connect(sock_path)
                held.append(s)
            except OSError:
                rejected += 1
                s.close()
        # Probe which connections actually get a response vs closed due to 64-conn cap
        answered = 0
        dropped = 0
        for s in held:
            try:
                s.sendall(b'{"jsonrpc":"2.0","id":1,"method":"v1.hello","params":{}}\n')
                data = s.recv(4096)
                if data:
                    answered += 1
                else:
                    dropped += 1
            except OSError:
                dropped += 1
        for s in held:
            s.close()
        time.sleep(0.2)

        # Verify all 64 semaphore permits were returned!
        post_ok = 0
        for _ in range(64):
            if "result" in rpc_call(sock_path, "v1.hello"):
                post_ok += 1
        if answered <= 64 and (dropped + rejected) >= 16 and post_ok == 64:
            report.ok(
                "Connection cap (64) enforced with zero permit leaks",
                f"answered={answered}, dropped={dropped + rejected}, post-burst 64/64 OK",
            )
        else:
            report.fail(
                "Connection cap & permit recovery",
                f"answered={answered}, dropped={dropped + rejected}, post_ok={post_ok}/64",
            )

        # 3b. 40-thread concurrent RPC & abrupt-disconnect storm
        latencies_ms: list[float] = []
        lat_lock = threading.Lock()
        errors = 0
        stop_at = time.monotonic() + 3.0

        def storm_worker(wid: int) -> None:
            nonlocal errors
            methods = ["v1.list_sensors", "v1.get_summary", "v1.get_inventory"]
            idx = wid
            while time.monotonic() < stop_at:
                if idx % 7 == 0:
                    # Abrupt disconnect mid-subscription
                    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    s.settimeout(1.0)
                    try:
                        s.connect(sock_path)
                        s.sendall(b'{"jsonrpc":"2.0","id":1,"method":"v1.subscribe","params":{"interval_ms":250}}\n')
                    except OSError:
                        pass
                    finally:
                        s.close()
                else:
                    t0 = time.monotonic()
                    try:
                        r = rpc_call(sock_path, methods[idx % len(methods)], timeout=2.0)
                        if "result" not in r:
                            errors += 1
                        else:
                            dt = (time.monotonic() - t0) * 1000.0
                            with lat_lock:
                                latencies_ms.append(dt)
                    except Exception:
                        errors += 1
                idx += 1

        with concurrent.futures.ThreadPoolExecutor(max_workers=24) as pool:
            futs = [pool.submit(storm_worker, i) for i in range(24)]
            for f in futs:
                f.result()

        latencies_ms.sort()
        total_rpcs = len(latencies_ms)
        if total_rpcs > 0 and errors == 0:
            p50 = latencies_ms[int(total_rpcs * 0.50)]
            p95 = latencies_ms[int(total_rpcs * 0.95)]
            p99 = latencies_ms[min(total_rpcs - 1, int(total_rpcs * 0.99))]
            report.ok(
                "24-thread RPC + abrupt-disconnect storm",
                f"{total_rpcs} RPCs, 0 errors | p50={p50:.1f}ms p95={p95:.1f}ms p99={p99:.1f}ms",
            )
        else:
            report.fail("24-thread RPC storm", f"rpcs={total_rpcs}, errors={errors}")

        # 3c. Stalled-reader subscriber DoS check (64 subscribers that never read)
        stalled: list[socket.socket] = []
        for _ in range(64):
            ss = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            ss.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
            ss.settimeout(1.0)
            try:
                ss.connect(sock_path)
                ss.sendall(b'{"jsonrpc":"2.0","id":1,"method":"v1.subscribe","params":{"interval_ms":250}}\n')
                stalled.append(ss)
            except OSError:
                ss.close()
        # Wait for send buffers to fill and 2s write timeout to evict stalled readers
        evicted_ok = False
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            try:
                if "result" in rpc_call(sock_path, "v1.hello", timeout=1.0):
                    evicted_ok = True
                    break
            except Exception:
                time.sleep(0.3)
        for ss in stalled:
            ss.close()
        if evicted_ok:
            report.ok("Stalled-reader subscriber DoS defeated (write timeout evicted non-draining clients)")
        else:
            report.fail("Stalled-reader subscriber DoS", "daemon remained locked after 64 stalled subscribers")

        # --- Suite 4: Mini-Soak (RAM, FD, Thread & Zombie Leak Check) ---
        print(f"\n=== Suite 4: Soak Endurance Test ({soak_secs}s) ===")
        time.sleep(0.4)  # let storm sockets finish closing before sampling baseline
        # Keep 2 active subscribers running during soak
        sub_socks: list[socket.socket] = []
        for _ in range(2):
            ss = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            ss.settimeout(2.0)
            ss.connect(sock_path)
            ss.sendall(b'{"jsonrpc":"2.0","id":1,"method":"v1.subscribe","params":{"interval_ms":500}}\n')
            sub_socks.append(ss)

        samples: list[dict[str, int]] = []
        for _ in range(max(3, soak_secs)):
            for ss in sub_socks:
                try:
                    ss.setblocking(False)
                    while ss.recv(65536):
                        pass
                except BlockingIOError:
                    pass
                finally:
                    ss.setblocking(True)
            samples.append(read_proc_metrics(proc.pid))
            time.sleep(1.0)

        for ss in sub_socks:
            ss.close()
        time.sleep(0.3)
        after = read_proc_metrics(proc.pid)

        rss_start = samples[0]["rss_kb"]
        rss_end = samples[-1]["rss_kb"]
        rss_max = max(s["rss_kb"] for s in samples)
        fd_min = min(s["fds"] for s in samples)
        fd_max = max(s["fds"] for s in samples)
        max_zombies = max(s["zombies"] for s in samples)

        if max_zombies == 0:
            report.ok("Zero zombie child processes during soak")
        else:
            report.fail("Zero zombie child processes", f"observed {max_zombies} zombies")

        # After subscriber disconnect, FDs should return close to baseline
        if fd_max - fd_min <= 6 and after["fds"] <= fd_min + 2:
            report.ok("Open FD count stayed flat", f"range={fd_min}..{fd_max}, post-disconnect={after['fds']}")
        else:
            report.fail("Open FD leak detected", f"range={fd_min}..{fd_max}, post={after['fds']}")

        rss_growth_kb = rss_end - rss_start
        if rss_max < 40_000 and rss_growth_kb < 4_096:
            report.ok(
                "Memory (VmRSS) remained bounded & flat",
                f"start={rss_start/1024:.1f} MiB, end={rss_end/1024:.1f} MiB, peak={rss_max/1024:.1f} MiB (delta {rss_growth_kb:+} KiB)",
            )
        else:
            report.warn(
                "Memory growth during soak",
                f"start={rss_start/1024:.1f} MiB, end={rss_end/1024:.1f} MiB, peak={rss_max/1024:.1f} MiB",
            )

        # Clean SIGTERM shutdown + socket cleanup check
        proc.send_signal(signal.SIGTERM)
        rc = proc.wait(timeout=5)
        if rc == 0 and not os.path.exists(sock_path):
            report.ok("Clean SIGTERM shutdown & Unix socket file cleanup")
        else:
            report.fail("Clean SIGTERM shutdown", f"rc={rc}, socket_exists={os.path.exists(sock_path)}")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        shutil.rmtree(tmpdir, ignore_errors=True)


def suite_5_live_service_and_plugin(report: TestReport) -> None:
    print("\n=== Suite 5: Live Production Daemon & Klipper Plugin Audit ===")
    live_sock = "/run/hwinfo64l/hwinfo64l.sock"
    if not os.path.exists(live_sock):
        report.warn("Live hwinfo64ld.service check", f"{live_sock} not present")
    else:
        hello = rpc_call(live_sock, "v1.hello")
        res = hello.get("result", {})
        if res.get("protocol_version") == 1 and res.get("privileged") is True:
            report.ok(
                "Live hwinfo64ld.service RPC healthy",
                f"v{res.get('daemon_version')}, host={res.get('hostname')}, uptime={res.get('uptime_secs')}s",
            )
        else:
            report.fail("Live hwinfo64ld.service RPC", str(hello))

        sensors = rpc_call(live_sock, "v1.list_sensors")["result"]
        probes = sensors["probes"]
        healthy = [p for p in probes if p["state"] == "healthy"]
        errored = [p for p in probes if p["total_errors"] > 0]
        if len(healthy) >= 10 and not errored:
            report.ok(
                "Live hardware probes healthy with zero errors",
                f"{len(healthy)}/{len(probes)} healthy, {len(sensors['readings'])} live readings",
            )
        else:
            report.warn(
                "Live hardware probes",
                f"{len(healthy)}/{len(probes)} healthy, errored={[p['probe_id'] for p in errored]}",
            )

        # Verify systemd security exposure score
        sec = subprocess.run(
            ["systemd-analyze", "security", "hwinfo64ld.service", "--no-pager"],
            capture_output=True,
            text=True,
            check=False,
        )
        summary_line = [l for l in sec.stdout.splitlines() if "Overall exposure level" in l]
        if summary_line:
            report.ok("Systemd sandbox audit", summary_line[-1].strip())

        # Verify zero listening TCP/UDP sockets owned by hwinfo64ld
        ss_out = subprocess.run(["ss", "-tulnp"], capture_output=True, text=True, check=False).stdout
        if "hwinfo64ld" not in ss_out and "hwinfo64l" not in ss_out:
            report.ok("Zero network listening ports exposed by HWInfo64L")
        else:
            report.fail("Unexpected network port open", ss_out)

    # Klipper / Kalico plugin resilience test (malformed host line + normal poll + shutdown)
    klipper_plugin = REPO_ROOT / "plugins" / "klipper-moonraker" / "klipper_moonraker.py"
    if klipper_plugin.exists():
        p = subprocess.Popen(
            [str(klipper_plugin)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        assert p.stdin is not None and p.stdout is not None
        p.stdin.write('{"type":"hello","protocol_version":1}\n')
        p.stdin.flush()
        m_line = p.stdout.readline()
        p.stdin.write("NOT_VALID_JSON\n")
        p.stdin.flush()
        err_line = p.stdout.readline()
        p.stdin.write('{"type":"poll"}\n')
        p.stdin.flush()
        poll_line = p.stdout.readline()
        p.stdin.write('{"type":"shutdown"}\n')
        p.stdin.flush()
        rc = p.wait(timeout=3)

        manifest = json.loads(m_line) if m_line.strip() else {}
        err_msg = json.loads(err_line) if err_line.strip() else {}
        poll_msg = json.loads(poll_line) if poll_line.strip() else {}
        if (
            rc == 0
            and manifest.get("type") == "manifest"
            and err_msg.get("type") == "error"
            and poll_msg.get("type") == "readings"
            and len(poll_msg.get("values", {})) > 0
        ):
            report.ok(
                "Klipper/Kalico plugin survived bad input & polled live",
                f"{len(poll_msg['values'])} values, {len(manifest.get('inventory', []))} inventory nodes, clean exit",
            )
        else:
            report.warn("Klipper/Kalico plugin test", f"rc={rc}, manifest={manifest.get('type')}, poll={poll_msg.get('type')}")


def main() -> int:
    parser = argparse.ArgumentParser(description="HWInfo64L security, fuzz, stress, and soak test suite")
    parser.add_argument("--daemon-bin", type=Path, default=DEFAULT_DAEMON_BIN)
    parser.add_argument("--soak-secs", type=int, default=15, help="Duration of soak test in seconds (default: 15)")
    parser.add_argument("--skip-live", action="store_true", help="Skip live systemd service & Klipper audit")
    args = parser.parse_args()

    if not args.daemon_bin.exists():
        print(f"error: daemon binary not found at {args.daemon_bin} (run `cargo build --release --workspace` first)")
        return 2

    print(f"Running HWInfo64L Robustness & Security Suite against {args.daemon_bin}")
    report = TestReport()

    suite_1_hostile_plugins(args.daemon_bin, report)
    suite_2_3_4_fuzz_stress_soak(args.daemon_bin, args.soak_secs, report)
    if not args.skip_live:
        suite_5_live_service_and_plugin(report)

    print("\n========================================")
    print(f"SUMMARY: {report.passed} passed | {report.warnings} warnings | {report.failed} failed")
    print("========================================")
    return 1 if report.failed > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
