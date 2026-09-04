{
  pkgs,
  genkan,
  productionGenkan,
}:

pkgs.runCommand "genkan-session-lock-smoke"
  {
    nativeBuildInputs = with pkgs; [
      coreutils
      gnugrep
      python3
      sway
      wlrctl
      wtype
    ];
  }
  ''
    trap 'echo "session-lock smoke failed at line $LINENO" >&2' ERR
    runtime=$(mktemp -d)
    config=$(mktemp)
    log=$(mktemp)
    lock_log=$(mktemp)
    production_log=$(mktemp)
    ready=$(mktemp)
    observer=$(mktemp)
    daemon_one=
    daemon_two=
    before_sleep=
    cleanup() {
      if [[ -n "''${lock_pid:-}" ]]; then
        kill "$lock_pid" 2>/dev/null || true
        wait "$lock_pid" 2>/dev/null || true
      fi
      if [[ -n "''${sway_pid:-}" ]]; then
        kill "$sway_pid" 2>/dev/null || true
        wait "$sway_pid" 2>/dev/null || true
      fi
      rm -rf "$runtime" "$config" "$log" "$lock_log" "$production_log" "$ready" "$observer"
      [[ -z "$daemon_one" ]] || rm -f "$daemon_one"
      [[ -z "$daemon_two" ]] || rm -f "$daemon_two"
      [[ -z "$before_sleep" ]] || rm -f "$before_sleep"
    }
    trap cleanup EXIT
    chmod 700 "$runtime"
    test -x ${productionGenkan}/libexec/genkan-lock-auth
    test ! -e ${productionGenkan}/bin/genkan-lock-auth
    [[ $(stat -c '%a' ${productionGenkan}/libexec/genkan-lock-auth) == 555 ]]
    python3 - ${productionGenkan}/libexec/genkan-lock-auth <<'PY'
    import os
    import pwd
    import select
    import signal
    import socket
    import struct
    import subprocess
    import sys

    parent, child = socket.socketpair()
    parent_pid = os.getpid()
    parent_fd = os.dup(parent.fileno())
    parent.close()
    parent = socket.socket(fileno=parent_fd)
    parent.settimeout(2)
    os.dup2(child.fileno(), 3)

    process = subprocess.Popen(
        [sys.argv[1], "--fd", "3", "--parent-pid", str(parent_pid)],
        pass_fds=(3,),
    )
    if child.fileno() != 3:
        os.close(3)
    child.close()

    def receive(length):
        result = b""
        while len(result) < length:
            part = parent.recv(length - len(result))
            assert part
            result += part
        return result

    header = receive(10)
    assert header[:6] == b"GNKA\x02\x01"
    payload = receive(struct.unpack(">I", header[6:])[0])
    assert struct.unpack(">I", payload[:4])[0] == os.getuid()
    assert payload[4:].decode() == pwd.getpwuid(os.getuid()).pw_name
    parent.close()
    assert process.wait(timeout=2) != 0

    reported, report = os.pipe()
    acknowledged, acknowledge = os.pipe()
    supervisor = os.fork()
    if supervisor == 0:
        os.close(reported)
        os.close(acknowledge)
        parent, child = socket.socketpair()
        parent_fd = os.dup(parent.fileno())
        parent.close()
        parent = socket.socket(fileno=parent_fd)
        parent.settimeout(2)
        os.dup2(child.fileno(), 3)
        worker = subprocess.Popen(
            [sys.argv[1], "--fd", "3", "--parent-pid", str(os.getpid())],
            pass_fds=(3,),
        )
        if child.fileno() != 3:
            os.close(3)
        child.close()
        header = receive(10)
        receive(struct.unpack(">I", header[6:])[0])
        os.kill(worker.pid, signal.SIGSTOP)
        os.write(report, str(worker.pid).encode())
        os.read(acknowledged, 1)
        os._exit(0)

    os.close(report)
    os.close(acknowledged)
    worker_pid = int(os.read(reported, 32))
    worker_pidfd = os.pidfd_open(worker_pid)
    os.write(acknowledge, b"1")
    os.close(acknowledge)
    os.waitpid(supervisor, 0)
    poll = select.poll()
    poll.register(worker_pidfd, select.POLLIN)
    assert poll.poll(2000), "worker survived its verified parent"
    os.close(worker_pidfd)
    PY
    if ${productionGenkan}/bin/genkan lock --test-unlock-after-ready > "$production_log" 2>&1; then
      echo "production package accepted the test-only unlock option" >&2
      exit 1
    fi
    grep -F -- '--test-unlock-after-ready' "$production_log"

    printf '%s\n' \
      'output * mode 800x600' \
      'seat * hide_cursor 1000' > "$config"

    XDG_RUNTIME_DIR="$runtime" \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/no-session-bus" \
      WLR_BACKENDS=headless \
      WLR_HEADLESS_OUTPUTS=2 \
      WLR_LIBINPUT_NO_DEVICES=1 \
      sway -c "$config" -d > "$log" 2>&1 &
    sway_pid=$!

    socket=
    for _ in $(seq 1 100); do
      socket=$(find "$runtime" -maxdepth 1 -type s -name 'wayland-*' -print -quit)
      [[ -n "$socket" ]] && break
      sleep 0.05
    done
    if [[ -z "$socket" ]]; then
      cat "$log" >&2
      exit 1
    fi

    ipc=
    for _ in $(seq 1 100); do
      ipc=$(find "$runtime" -maxdepth 1 -type s -name 'sway-ipc.*.sock' -print -quit)
      [[ -n "$ipc" ]] && break
      sleep 0.05
    done
    if [[ -z "$ipc" ]]; then
      cat "$log" >&2
      exit 1
    fi

    env \
      WAYLAND_DISPLAY=$(basename "$socket") \
      XDG_RUNTIME_DIR="$runtime" \
      timeout 30s ${genkan}/bin/genkan lock \
        --reduce-motion \
        --test-unlock-after-ready \
        --test-observer-fd 4 \
        --ready-fd 3 \
        3>"$ready" 4>"$observer" 2>"$lock_log" &
    lock_pid=$!

    for _ in $(seq 1 300); do
      grep -Fxq READY "$ready" && break
      ! kill -0 "$lock_pid" 2>/dev/null && break
      sleep 0.01
    done
    if ! grep -Fxq READY "$ready"; then
      echo "foreground lock did not report readiness" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    grep -Fxq LOCKED "$observer"
    initial_geometry=$(grep -Fc GEOMETRY "$observer")
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") \
      swaymsg -s "$ipc" output HEADLESS-2 scale 2 >/dev/null
    for _ in $(seq 1 300); do
      [[ $(grep -Fc GEOMETRY "$observer") -gt $initial_geometry ]] && break
      sleep 0.01
    done
    if [[ $(grep -Fc GEOMETRY "$observer") -le $initial_geometry ]]; then
      echo "mixed output scaling did not reach the lock runtime" >&2
      cat "$log" "$lock_log" "$observer" >&2
      exit 1
    fi
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wtype -s 50 x
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wlrctl pointer move 1 1
    XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=$(basename "$socket") wlrctl pointer click
    for _ in $(seq 1 300); do
      grep -Fxq KEYBOARD "$observer" && grep -Fxq POINTER "$observer" && break
      sleep 0.01
    done
    if ! grep -Fxq KEYBOARD "$observer" || ! grep -Fxq POINTER "$observer"; then
      echo "input observer did not receive isolated keyboard and pointer input" >&2
      cat "$log" "$lock_log" "$observer" >&2
      exit 1
    fi

    if ! XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" create_output >/dev/null; then
      echo "could not create headless output" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    for _ in $(seq 1 300); do
      [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -ge 3 ]] && break
      sleep 0.01
    done
    if [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -lt 3 ]]; then
      echo "added output did not receive an opaque frame" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    if ! XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-3 disable >/dev/null; then
      XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" -t get_outputs >&2 || true
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    for _ in $(seq 1 300); do
      [[ $(grep -Fc 'removed surface for output' "$lock_log") -ge 1 ]] && break
      sleep 0.01
    done
    if [[ $(grep -Fc 'removed surface for output' "$lock_log") -lt 1 ]]; then
      echo "removed output retained its lock surface" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    if ! XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-3 enable >/dev/null; then
      echo "could not re-enable headless output" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    for _ in $(seq 1 300); do
      [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -ge 4 ]] && break
      sleep 0.01
    done
    if [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -lt 4 ]]; then
      echo "re-enabled output did not receive an opaque frame" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi

    if ! wait "$lock_pid"; then
      echo "foreground lock did not complete its test unlock" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    lock_pid=
    if [[ $(grep -Fc 'created lock surface for output' "$lock_log") -lt 4 ]] ||
       [[ $(grep -Fc 'committed first opaque buffer for output' "$lock_log") -lt 4 ]] ||
       [[ $(grep -Fc 'removed surface for output' "$lock_log") -lt 1 ]]; then
      echo "foreground output lifecycle counts were incomplete" >&2
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    grep -Fxq OUTPUT_REMOVED "$observer"
    if grep -Ev '^(LOCKED|FAILED|FINISHED|OUTPUT_ADDED|OUTPUT_REMOVED|GEOMETRY|KEYBOARD|POINTER|AUTH_PROMPT|AUTH_RETRY|AUTH_SUCCESS|AUTH_FAILURE)$' "$observer"; then
      echo "observer emitted data outside its fixed non-secret vocabulary" >&2
      cat "$observer" >&2
      exit 1
    fi

    daemon_one=$(mktemp)
    daemon_two=$(mktemp)
    before_sleep=$(mktemp)
    rm -f "$before_sleep"
    env WAYLAND_DISPLAY=$(basename "$socket") XDG_RUNTIME_DIR="$runtime" \
      ${genkan}/bin/genkan lock --daemonize --reduce-motion --test-unlock-after-ready \
      2>"$daemon_one" &
    daemon_one_pid=$!
    for _ in $(seq 1 100); do
      find "$runtime" -maxdepth 1 -type s -name 'genkan-lock-*.sock' | grep -q . && break
      sleep 0.01
    done
    if ! find "$runtime" -maxdepth 1 -type s -name 'genkan-lock-*.sock' | grep -q .; then
      echo "daemonized foreground child did not publish its coordination socket" >&2
      cat "$daemon_one" >&2
      exit 1
    fi
    env WAYLAND_DISPLAY=$(basename "$socket") XDG_RUNTIME_DIR="$runtime" \
      ${genkan}/bin/genkan lock --daemonize --reduce-motion --test-unlock-after-ready \
      2>"$daemon_two" &
    daemon_two_pid=$!
    if ! wait "$daemon_one_pid"; then
      echo "first readiness launcher failed" >&2
      cat "$daemon_one" "$daemon_two" >&2
      exit 1
    fi
    if ! wait "$daemon_two_pid"; then
      echo "duplicate readiness launcher failed" >&2
      cat "$daemon_one" "$daemon_two" >&2
      exit 1
    fi
    touch "$before_sleep"
    test -e "$before_sleep"
    if [[ $(cat "$daemon_one" "$daemon_two" | grep -Fc 'compositor confirmed lock') -ne 1 ]]; then
      echo "duplicate launchers did not join exactly one protocol lock" >&2
      cat "$log" "$daemon_one" "$daemon_two" >&2
      exit 1
    fi
    for _ in $(seq 1 700); do
      ! find "$runtime" -maxdepth 1 -type s -name 'genkan-lock-*.sock' | grep -q . && break
      sleep 0.01
    done
    if find "$runtime" -maxdepth 1 -type s -name 'genkan-lock-*.sock' | grep -q .; then
      echo "daemonized foreground child did not clean up after test unlock" >&2
      cat "$daemon_one" "$daemon_two" >&2
      exit 1
    fi

    if env WAYLAND_DISPLAY=missing XDG_RUNTIME_DIR="$runtime" \
      timeout 5s ${genkan}/bin/genkan lock --daemonize --reduce-motion \
      > /dev/null 2>&1; then
      echo "daemon launcher accepted a missing compositor before readiness" >&2
      exit 1
    fi
    rm -f "$daemon_one" "$daemon_two" "$before_sleep"
    touch "$out"
  ''
