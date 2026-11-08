{ pkgs, genkan }:
{
  name = "genkan-niri-greeter";
  enableOCR = true;
  nodes.machine =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      # No credentials in this test. A PAM conversation holds authentication
      # until Return; greetd, not the supervisor, authorizes the session.
      confirmationPam =
        pkgs.runCommand "pam-genkan-confirm"
          {
            nativeBuildInputs = [ pkgs.stdenv.cc ];
            buildInputs = [ pkgs.pam ];
          }
          ''
            mkdir -p "$out/lib/security"
            cat > confirm.c <<'EOF'
            #include <security/pam_appl.h>
            #include <security/pam_modules.h>
            #include <stdlib.h>
            #include <string.h>
            PAM_EXTERN int pam_sm_authenticate(pam_handle_t *p, int f, int n, const char **a) {
              (void)f; (void)n; (void)a;
              const struct pam_conv *c = NULL;
              if (pam_get_item(p, PAM_CONV, (const void **)&c) != PAM_SUCCESS || !c) return PAM_SYSTEM_ERR;
              const struct pam_message m = { PAM_PROMPT_ECHO_OFF, "Confirm" };
              const struct pam_message *mp = &m;
              struct pam_response *r = NULL;
              int result = c->conv(1, &mp, &r, c->appdata_ptr);
              if (r) { if (r->resp) { memset(r->resp, 0, strlen(r->resp)); free(r->resp); } free(r); }
              return result;
            }
            PAM_EXTERN int pam_sm_setcred(pam_handle_t *p, int f, int n, const char **a) {
              (void)p; (void)f; (void)n; (void)a; return PAM_SUCCESS;
            }
            EOF
            cc -shared -fPIC -Wall -Wextra -Werror confirm.c -lpam -o "$out/lib/security/confirm.so"
          '';
      sentinel = pkgs.writeShellScript "default-bind-sentinel" ''
        echo "unexpected default spawn" >> /tmp/genkan-default-spawn
      '';
      sentinels = pkgs.runCommand "default-bind-sentinels" { } ''
        mkdir -p "$out/bin"
        for name in alacritty fuzzel swaylock orca pkill wpctl playerctl brightnessctl waybar; do
          ln -s ${sentinel} "$out/bin/$name"
        done
      '';
      inputPython = pkgs.python3.withPackages (p: [ p.evdev ]);
      inputDriver = pkgs.writeText "genkan-physical-keyboard.py" ''
        import json
        import socket
        import sys
        from evdev import UInput, ecodes

        path = "/run/genkan-test-input.sock"
        if len(sys.argv) > 1:
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(path)
                client.sendall(json.dumps([int(key) for key in sys.argv[1:]]).encode())
                assert client.recv(2) == b"ok"
        else:
            # A persistent kernel keyboard, not Wayland's virtual-keyboard
            # protocol (which bypasses niri's shortcut processing).
            with UInput({ecodes.EV_KEY: list(range(1, 256))}, name="Genkan test keyboard") as keyboard:
                with socket.socket(socket.AF_UNIX) as server:
                    server.bind(path)
                    server.listen()
                    while True:
                        connection, _ = server.accept()
                        with connection:
                            keys = json.loads(connection.recv(4096))
                            for key in keys:
                                keyboard.write(ecodes.EV_KEY, key, 1)
                                keyboard.syn()
                            for key in reversed(keys):
                                keyboard.write(ecodes.EV_KEY, key, 0)
                                keyboard.syn()
                            connection.sendall(b"ok")
      '';
      desktopConfig = pkgs.writeText "genkan-test-desktop.conf" ''
        output * mode 1280x960
      '';
      desktop = pkgs.writeShellScript "genkan-test-desktop" ''
        echo $$ > /tmp/genkan-desktop.pid
        id -un > /tmp/genkan-desktop.user
        export WLR_RENDERER=pixman
        export SWAYSOCK=/tmp/genkan-desktop.sock
        exec ${pkgs.sway-unwrapped}/bin/sway -c ${desktopConfig}
      '';
      session =
        (pkgs.writeTextDir "share/wayland-sessions/genkan-test.desktop" ''
          [Desktop Entry]
          Type=Application
          Name=Genkan test desktop
          Exec=${desktop}
        '').overrideAttrs
          { passthru.providedSessions = [ "genkan-test" ]; };
    in
    {
      imports = [ ../module.nix ];
      programs.genkan = {
        enable = true;
        package = genkan;
        greeter = {
          enable = true;
          compositor = "niri";
          authenticationOutput = "Virtual-1";
          reduceMotion = true;
          niri.outputs = {
            "Virtual-1" = {
              # Use a mode advertised by virtio-gpu; 1280x800 is absent.
              mode = "1280x960";
              scale = 1.5;
              position = {
                x = 0;
                y = 0;
              };
            };
            "Virtual-2" = {
              mode = "1280x960";
              scale = 1;
              position = {
                x = 854;
                y = 0;
              };
            };
          };
        };
      };
      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        hashedPassword = "";
      };
      security.pam.services.greetd.rules.auth.confirm = {
        order = 0;
        control = "sufficient";
        modulePath = "${confirmationPam}/lib/security/confirm.so";
      };
      services.displayManager.sessionPackages = [ session ];
      systemd.services.greetd = {
        path = [ sentinels ];
        environment.ICED_BACKEND = "tiny-skia";
      };
      boot.kernelModules = [ "uinput" ];
      system.build.genkanTestInput = pkgs.writeShellScript "genkan-test-input" ''
        exec ${inputPython}/bin/python ${inputDriver} "$@"
      '';
      system.build.genkanTestSentinel = sentinel;
      environment.systemPackages = [
        pkgs.niri
        pkgs.sway
        pkgs.jq
        pkgs.imagemagick
      ];
      # virgl is required: upstream niri deliberately rejects software EGL
      # devices on its DRM backend. Host GTK/GL runs in a private Xvfb below,
      # using llvmpipe, so the test needs no physical GPU.
      virtualisation.qemu.package = lib.mkForce pkgs.qemu;
      virtualisation.qemu.options = [
        "-vga none"
        "-device virtio-vga-gl,max_outputs=2"
        "-display gtk,gl=on"
      ];
      virtualisation.memorySize = 2048;
      virtualisation.cores = 2;
      specialisation.rebuilt.configuration.programs.genkan.greeter.wallpaper = lib.mkForce "golden-gate";
      system.stateVersion = "26.05";
    };

  testScript = { nodes, ... }: ''
    from datetime import timedelta
    import json
    import os
    from pathlib import Path
    import shlex
    import subprocess
    import time

    os.environ.update({
        "DISPLAY": ":93", "LIBGL_ALWAYS_SOFTWARE": "1",
        "LIBGL_DRIVERS_PATH": "${pkgs.mesa.drivers}/lib/dri",
        "__EGL_VENDOR_LIBRARY_FILENAMES": "${pkgs.mesa.drivers}/share/glvnd/egl_vendor.d/50_mesa.json",
    })
    display = subprocess.Popen(["${pkgs.xorg.xorgserver}/bin/Xvfb", ":93", "-screen", "0", "2560x1600x24", "-nolisten", "tcp", "-ac"])
    try:
        for _ in range(200):
            if Path("/tmp/.X11-unix/X93").exists():
                break
            time.sleep(0.05)
        assert display.poll() is None
        machine.start()
        machine.wait_for_unit("greetd.service")
        uid = machine.succeed("id -u greeter").strip()
        runtime = f"/run/user/{uid}"

        def greeter(command):
            socket = f"$(find {runtime} -maxdepth 1 -name 'niri.*.sock' -print -quit)"
            wayland = f"$(basename {socket} | cut -d. -f2)"
            return f"runuser -u greeter -- env XDG_RUNTIME_DIR={runtime} NIRI_SOCKET={socket} WAYLAND_DISPLAY={wayland} {command}"

        def msg(command):
            return json.loads(machine.succeed(greeter(f"niri msg -j {command}")))

        def wait_login():
            machine.wait_until_succeeds(greeter("niri msg -j windows | jq -e 'any(.[]; .app_id == \"genkan\")'"), timeout=timedelta(seconds=90))
            return next(window for window in msg("windows") if window["app_id"] == "genkan")

        def greeter_screenshot(name):
            # QEMU's generic screendump cannot read back virgl scanouts.
            path = f"/tmp/{name}.png"
            machine.succeed(f"rm -f {path}")
            machine.succeed(greeter(f"niri msg action screenshot-screen --path {path}"))
            machine.wait_until_succeeds(f"test -s {path}")
            machine.copy_from_machine(path)
            return machine.out_dir / f"{name}.png"

        # Linux evdev codes for every spawn binding in niri 26.04's defaults.
        shortcuts = {
            "Mod+T": [125, 20], "Mod+D": [125, 32],
            "Mod+Alt+L": [125, 56, 38], "Mod+Alt+S": [125, 56, 31],
            "XF86AudioRaiseVolume": [115], "XF86AudioLowerVolume": [114],
            "XF86AudioMute": [113], "XF86AudioMicMute": [248],
            "XF86AudioPlay": [164], "XF86AudioStop": [166],
            "XF86AudioPrev": [165], "XF86AudioNext": [163],
            "XF86MonBrightnessUp": [225], "XF86MonBrightnessDown": [224],
        }
        input_driver = "${nodes.machine.system.build.genkanTestInput}"
        machine.execute(input_driver + " >/tmp/input.log 2>&1 &")
        machine.wait_until_succeeds("test -S /run/genkan-test-input.sock")

        # Positive control: the very same kernel input must fire every binding
        # before its absence in the hardened configuration is meaningful.
        control = Path("${nodes.machine.programs.genkan.greeter.niri.configFile}").read_text()
        binds = "\n".join(f'{key} {{ spawn "${nodes.machine.system.build.genkanTestSentinel}"; }}' for key in shortcuts)
        control = control.replace("binds {}", "binds {\n" + binds + "\n}")
        machine.succeed("systemctl stop greetd")
        machine.succeed("printf %s " + shlex.quote(control) + " >/tmp/control.kdl")
        machine.succeed("niri validate --config /tmp/control.kdl")
        control_greetd = '[terminal]\nvt=1\n[default_session]\nuser="greeter"\ncommand="${nodes.machine.programs.genkan.greeter.niri.package}/bin/niri --config /tmp/control.kdl"\n'
        machine.succeed("printf %s " + shlex.quote(control_greetd) + " >/tmp/control.toml")
        override = '[Service]\nExecStart=\nExecStart=${pkgs.greetd}/bin/greetd --config /tmp/control.toml\n'
        machine.succeed("mkdir -p /run/systemd/system/greetd.service.d; printf %s " + shlex.quote(override) + " >/run/systemd/system/greetd.service.d/control.conf; systemctl daemon-reload; systemctl start greetd")
        wait_login()
        for count, keys in enumerate(shortcuts.values(), 1):
            machine.succeed(input_driver + " " + " ".join(map(str, keys)))
            machine.wait_until_succeeds(f"test $(wc -l </tmp/genkan-default-spawn) -ge {count}")
        machine.succeed("systemctl stop greetd; rm /run/systemd/system/greetd.service.d/control.conf /tmp/genkan-default-spawn; systemctl daemon-reload; systemctl start greetd")

        login = wait_login()
        outputs = msg("outputs")
        assert set(outputs) == {"Virtual-1", "Virtual-2"}, outputs
        assert outputs["Virtual-1"]["logical"]["scale"] == 1.5
        for output in outputs.values():
            mode = output["modes"][output["current_mode"]]
            assert (mode["width"], mode["height"]) == (1280, 960), output
        assert login["layout"]["window_size"] == [853, 640], login
        assert login["layout"]["window_offset_in_tile"] == [0.0, 0.0], login
        workspace = next(w for w in msg("workspaces") if w["id"] == login["workspace_id"])
        assert workspace["output"] == "Virtual-1", workspace
        machine.wait_until_succeeds(greeter("niri msg -j layers | jq -e 'any(.[]; .namespace == \"genkan-wallpaper\")'"))
        layers = [layer for layer in msg("layers") if layer["namespace"] == "genkan-wallpaper"]
        assert [layer["output"] for layer in layers] == ["Virtual-2"], layers
        assert all(layer["keyboard_interactivity"].lower() == "none" for layer in layers)
        greeter_screenshot("niri-login")
        machine.succeed(greeter("niri msg action focus-monitor Virtual-2"))
        greeter_screenshot("genkan-wallpaper")
        assert int(machine.succeed("magick /tmp/genkan-wallpaper.png -format '%k' info:")) > 256
        machine.succeed(greeter("niri msg action focus-monitor Virtual-1"))

        # Every default command is installed as a sentinel, including media
        # controls; missing terminal binaries cannot make this a false green.
        for keys in shortcuts.values():
            machine.succeed(input_driver + " " + " ".join(map(str, keys)))
        machine.sleep(timedelta(seconds=2))
        machine.fail("test -e /tmp/genkan-default-spawn")
        machine.fail("pgrep -u greeter -f Xwayland")

        first_niri = machine.succeed("pgrep -u greeter -x niri").strip()
        machine.succeed(f"kill -KILL {login['pid']}")
        machine.wait_until_fails(f"kill -0 {first_niri}")
        login = wait_login()
        assert machine.succeed("pgrep -u greeter -x niri").strip() != first_niri

        # Mapping a window does not establish PAM readiness. Wait for the
        # rendered prompt before sending Return; no response is logged.
        def prompt_visible(_last_try):
            image = greeter_screenshot("pam-prompt")
            # The mode and scale asserted above put the input here. Bound OCR
            # to that line, not the busy wallpaper; discard screenshot alpha
            # before inversion so OCR never receives a transparent image.
            line = image.with_name("pam-input.png")
            subprocess.run([
                "${pkgs.imagemagick}/bin/magick", str(image), "-alpha", "off",
                "-crop", "220x48+530+735", "+repage", "-resize", "400%",
                "-colorspace", "Gray", "-negate", str(line),
            ], check=True, timeout=10)
            ocr = subprocess.run([
                "tesseract", str(line), "stdout", "--oem", "1", "--psm", "7",
                "-c", "debug_file=/dev/null",
            ], check=True, capture_output=True, text=True, timeout=10)
            return "confirm" in ocr.stdout.casefold().split()

        retry(prompt_visible, timedelta(seconds=90))
        assert msg("focused-window")["app_id"] == "genkan"
        machine.send_key("ret")
        machine.wait_until_succeeds("test -S /tmp/genkan-desktop.sock", timeout=timedelta(seconds=90))
        machine.wait_until_fails("pgrep -u greeter -x niri")
        machine.succeed("grep -Fx alice /tmp/genkan-desktop.user")
        desktop_pid = machine.succeed("cat /tmp/genkan-desktop.pid").strip()
        greetd_pid = machine.succeed("systemctl show greetd -p MainPID --value").strip()
        desktop_outputs = machine.succeed("swaymsg -s /tmp/genkan-desktop.sock -t get_outputs")
        assert len(json.loads(desktop_outputs)) == 2
        machine.succeed("${nodes.machine.system.build.toplevel}/specialisation/rebuilt/bin/switch-to-configuration switch", timeout=timedelta(seconds=120))
        assert machine.succeed("systemctl show greetd -p MainPID --value").strip() == greetd_pid
        assert machine.succeed("cat /tmp/genkan-desktop.pid").strip() == desktop_pid
        machine.succeed(f"kill -0 {desktop_pid}")
        assert json.loads(machine.succeed("swaymsg -s /tmp/genkan-desktop.sock -t get_outputs")) == json.loads(desktop_outputs)
        machine.succeed("runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=$(find /run/user/1000 -maxdepth 1 -type s -name 'wayland-*' -print -quit) ${pkgs.grim}/bin/grim -o Virtual-1 /tmp/desktop-survived-generation-switch.png")
        machine.copy_from_machine("/tmp/desktop-survived-generation-switch.png")
    finally:
        # QEMU's GTK display exits with Xvfb. Finish the guest before removing
        # its display, including on failure, so the driver can cleanly sync.
        try:
            machine.shutdown()
        finally:
            display.terminate()
            display.wait(timeout=10)
  '';
}
