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
      sway
    ];
  }
  ''
    runtime=$(mktemp -d)
    config=$(mktemp)
    log=$(mktemp)
    lock_log=$(mktemp)
    production_log=$(mktemp)
    ready=$(mktemp)
    cleanup() {
      if [[ -n "''${lock_pid:-}" ]]; then
        kill "$lock_pid" 2>/dev/null || true
        wait "$lock_pid" 2>/dev/null || true
      fi
      if [[ -n "''${sway_pid:-}" ]]; then
        kill "$sway_pid" 2>/dev/null || true
        wait "$sway_pid" 2>/dev/null || true
      fi
      rm -rf "$runtime" "$config" "$log" "$lock_log" "$production_log" "$ready"
    }
    trap cleanup EXIT
    chmod 700 "$runtime"
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
        --ready-fd 3 \
        3>"$ready" 2>"$lock_log" &
    lock_pid=$!

    for _ in $(seq 1 100); do
      grep -Fxq READY "$ready" && break
      ! kill -0 "$lock_pid" 2>/dev/null && break
      sleep 0.01
    done
    if ! grep -Fxq READY "$ready"; then
      cat "$log" "$lock_log" >&2
      exit 1
    fi

    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" create_output >/dev/null
    for _ in $(seq 1 100); do
      [[ $(grep -Fc 'presented opaque buffer for output' "$lock_log") -ge 3 ]] && break
      sleep 0.01
    done
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-3 disable >/dev/null
    for _ in $(seq 1 100); do
      [[ $(grep -Fc 'removed surface for output' "$lock_log") -ge 1 ]] && break
      sleep 0.01
    done
    XDG_RUNTIME_DIR="$runtime" swaymsg -s "$ipc" output HEADLESS-3 enable >/dev/null
    for _ in $(seq 1 100); do
      [[ $(grep -Fc 'presented opaque buffer for output' "$lock_log") -ge 4 ]] && break
      sleep 0.01
    done

    if ! wait "$lock_pid"; then
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    lock_pid=
    if [[ $(grep -Fc 'created lock surface for output' "$lock_log") -lt 4 ]] ||
       [[ $(grep -Fc 'presented opaque buffer for output' "$lock_log") -lt 4 ]] ||
       [[ $(grep -Fc 'removed surface for output' "$lock_log") -lt 1 ]]; then
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    touch "$out"
  ''
