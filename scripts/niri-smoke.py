#!/usr/bin/env python3
"""Real niri winit clients nested inside headless Sway; no credentials entered."""
import glob
import json
import math
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import tempfile
import time


def wait_for(check, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("timed out waiting for niri/client state")


with tempfile.TemporaryDirectory(prefix="genkan-niri-") as directory:
    runtime = Path(directory)
    env = os.environ | {
        "XDG_RUNTIME_DIR": directory,
        "WLR_BACKENDS": "headless",
        "WLR_RENDERER": "pixman",
        "WLR_LIBINPUT_NO_DEVICES": "1",
        "LIBGL_ALWAYS_SOFTWARE": "1",
        "ICED_BACKEND": "tiny-skia",
    }
    children = []
    logs = []

    def start(arguments, name, extra=None):
        log = runtime / (name + ".log")
        logs.append(log)
        with log.open("w") as output:
            process = subprocess.Popen(arguments, env=env | (extra or {}), stdout=output, stderr=output)
        children.append(process)
        return process, log

    def stop(process):
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()

    def ipc(request):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(2)
            connection.connect(env["NIRI_SOCKET"])
            connection.sendall((json.dumps(request) + "\n").encode())
            return json.loads(connection.makefile().readline())["Ok"]

    def output():
        return ipc("Outputs")["Outputs"]["winit"]["logical"]

    def niri_start():
        process, _ = start([os.environ["NIRI_BIN"], "--config", str(runtime / "niri.kdl")], "niri")
        env["NIRI_SOCKET"] = wait_for(lambda: next(iter(glob.glob(directory + "/niri.*.sock")), None))
        env["WAYLAND_DISPLAY"] = Path(env["NIRI_SOCKET"]).name.split(".")[1]
        wait_for(lambda: output()["width"] == 853)
        return process

    def screenshot(name):
        # The outer compositor can capture the lock without granting an ordinary
        # client a screenshot capability inside the locked niri session.
        subprocess.run(["grim", str(Path(os.environ["out"]) / name)], env=outer_env, check=True)

    try:
        (runtime / "sway.conf").write_text(
            'output * mode 1280x800\nfor_window [app_id="niri"] fullscreen enable\n'
        )
        start([os.environ["SWAY_BIN"], "--config", str(runtime / "sway.conf")], "sway")
        outer = wait_for(lambda: next(iter(glob.glob(directory + "/wayland-*")), None))
        outer_env = env | {"WAYLAND_DISPLAY": outer}
        env["WAYLAND_DISPLAY"] = outer
        (runtime / "niri.kdl").write_text('''
output "winit" { scale 1.5; }
binds {}
hotkey-overlay { skip-at-startup; }
screenshot-path null
xwayland-satellite { off; }
layout { background-color "transparent"; }
layer-rule { match namespace="^genkan-wallpaper$"; place-within-backdrop true; }
window-rule { match app-id="^genkan$"; open-fullscreen true; }
''')
        niri = niri_start()
        binary = os.environ["GENKAN_BIN"]
        for index, (scale, selection) in enumerate([
            (1.5, ["--wallpaper", "golden-gate", "--reduce-motion"]),
            (1.5, ["--wallpaper", "tahoe-beach"]),
            (1.5, ["--wallpaper-file", os.environ["HEIC_FIXTURE"], "--reduce-motion"]),
            (1.275, ["--reduce-motion"]), (1.3, ["--reduce-motion"]),
            (1.2, ["--reduce-motion"]), (1, ["--reduce-motion"]),
        ]):
            subprocess.run([os.environ["NIRI_BIN"], "msg", "output", "winit", "scale", str(scale)], env=env, check=True)
            wait_for(lambda: abs(output()["scale"] - scale) < 0.001)
            # niri's IPC truncates logical output dimensions, but its layer
            # configure rounds them. Render against the actual surface size.
            logical = (round(1280 / scale), round(800 / scale))
            process, log = start([binary, "wallpaper"] + selection, f"wallpaper-{index}", {"WAYLAND_DEBUG": "client"})

            def committed():
                trace = log.read_text()
                return ".attach(" in trace and ".commit()" in trace.rsplit(".attach(", 1)[-1]

            wait_for(committed)
            if index == 1:
                # A poster fallback is not evidence that MOV playback works.
                wait_for(lambda: log.read_text().count(".attach(") >= 2)
            trace = log.read_text()
            assert process.poll() is None, trace
            assert "wallpaper playback stopped" not in trace, trace
            assert f"preferred_scale({round(scale * 120)})" in trace, trace
            assert ".set_buffer_scale(1)" in trace, trace
            assert f".set_destination({logical[0]}, {logical[1]})" in trace, trace
            sizes = re.findall(r"create_buffer\(new id wl_buffer#\d+, 0, (\d+), (\d+),", trace)
            assert tuple(map(int, sizes[-1])) == (
                math.ceil(logical[0] * scale), math.ceil(logical[1] * scale)
            ), (sizes, logical)
            if index == 0:
                time.sleep(1)
                screenshot("wallpaper-1.5.png")
            stop(process)
            print(f"wallpaper {index}: exact buffers at {scale}", flush=True)

        for requested in ["winit", "smithay winit Unknown"]:
            process, log = start([binary, "wallpaper", "--skip-output", requested], "skipped", {"WAYLAND_DEBUG": "client"})
            time.sleep(1)
            trace = log.read_text()
            assert process.poll() is None, trace
            assert ".get_layer_surface(" not in trace, trace
            assert ".create_buffer(" not in trace, trace
            stop(process)

        subprocess.run([os.environ["NIRI_BIN"], "msg", "output", "winit", "scale", "1.5"], env=env, check=True)
        wait_for(lambda: output()["width"] == 853)
        process, log = start([
            os.environ["GENKAN_LOCK_TEST_BIN"], "lock", "--reduce-motion",
            "--authentication-output", "Smithay Winit Unknown",
            "--test-unlock-after-ready", "--test-unlock-delay-ms", "15000",
        ], "lock", {"WAYLAND_DEBUG": "client"})
        wait_for(lambda: ".set_destination(853, 533)" in log.read_text())
        trace = log.read_text()
        assert "preferred_scale(180)" in trace, trace
        assert re.search(r"create_buffer\(new id wl_buffer#\d+, 0, 1280, 800,", trace), trace
        time.sleep(2)
        screenshot("lock-1.5.png")
        process.wait(timeout=30)
        assert process.returncode == 0, log.read_text()
        print("lock: fractional buffers, compositor confirmation and unlock passed", flush=True)

        for action in ["login-crash", "supervisor-signal"]:
            process, log = start([binary, "greeter-session", "--reduce-motion", "--authentication-output", "winit"], action)

            def login_pid():
                path = Path(f"/proc/{process.pid}/task/{process.pid}/children")
                if not path.exists():
                    return None
                for pid in path.read_text().split():
                    command = Path(f"/proc/{pid}/cmdline")
                    if command.exists() and b"login" in command.read_bytes().split(b"\0"):
                        return int(pid)
                return None

            login = wait_for(login_pid)
            os.kill(login if action == "login-crash" else process.pid,
                    signal.SIGKILL if action == "login-crash" else signal.SIGTERM)
            process.wait(timeout=10)
            niri.wait(timeout=10)
            assert not Path(f"/proc/{login}").exists()
            print(f"{action}: children reaped and niri exited", flush=True)
            if action == "login-crash":
                env["WAYLAND_DISPLAY"] = outer
                wait_for(lambda: not Path(env["NIRI_SOCKET"]).exists())
                niri = niri_start()
    except BaseException:
        for log in logs:
            print(f"--- {log.name} ---\n{log.read_text()[-12000:]}")
        raise
    finally:
        for process in reversed(children):
            stop(process)
