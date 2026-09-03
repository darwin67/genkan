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

    if ! env \
      WAYLAND_DISPLAY=$(basename "$socket") \
      XDG_RUNTIME_DIR="$runtime" \
      timeout 30s ${genkan}/bin/genkan lock \
        --reduce-motion \
        --test-unlock-after-ready \
        --ready-fd 3 \
        3>"$ready" 2>"$lock_log"; then
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    grep -Fx READY "$ready"
    if [[ $(grep -Fc 'created opaque surface for output' "$lock_log") -ne 2 ]]; then
      cat "$log" "$lock_log" >&2
      exit 1
    fi
    touch "$out"
  ''
