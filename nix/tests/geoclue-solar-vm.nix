{ genkan, fixture }:
{
  name = "genkan-geoclue-solar";

  nodes.machine =
    { config, pkgs, ... }:
    {
      imports = [ ../module.nix ];

      programs.genkan = {
        enable = true;
        package = genkan;
        wallpaper = {
          enable = true;
          solar.enable = true;
        };
      };
      # Deterministic host-owned location source for the pinned daemon.
      services.geoclue2.enableStatic = true;
      services.geoclue2.staticLatitude = 37.7749;
      services.geoclue2.staticLongitude = -122.4194;
      services.geoclue2.staticAltitude = 0;
      services.geoclue2.staticAccuracy = 1000;

      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      environment.variables.GENKAN_GEOCLUE_AGENT =
        "${config.services.geoclue2.package}/libexec/geoclue-2.0/demos/agent";
      environment.systemPackages = with pkgs; [
        gnugrep
        jq
        sway
      ];
      virtualisation.memorySize = 2048;
      system.stateVersion = "26.05";
    };

  testScript = ''
    from datetime import timedelta

    runtime = "/run/user/1000"
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("dbus.service")

    # The packaged user-session agent is what GeoClue consults before allowing
    # access. Start it in alice's user manager, as the module provisions it.
    machine.succeed("systemctl start user@1000.service")
    machine.wait_for_unit("user@1000.service")
    machine.succeed(
        "runuser -u alice -- env XDG_RUNTIME_DIR=%s "
        "systemctl --user start geoclue-agent" % runtime
    )
    machine.wait_until_succeeds(
        "runuser -u alice -- env XDG_RUNTIME_DIR=%s "
        "systemctl --user is-active geoclue-agent" % runtime
    )

    machine.succeed("install -d -m 0700 -o alice -g users %s" % runtime)
    machine.succeed(
        "printf 'output * mode 800x600\\nseat * hide_cursor 1000\\n' > /tmp/sway.conf"
    )
    machine.execute(
        "runuser -u alice -- env XDG_RUNTIME_DIR=%s WLR_BACKENDS=headless "
        "WLR_HEADLESS_OUTPUTS=1 WLR_LIBINPUT_NO_DEVICES=1 "
        "sway -c /tmp/sway.conf -d >/tmp/sway.log 2>&1 &" % runtime
    )
    machine.wait_until_succeeds(
        "find %s -maxdepth 1 -type s -name 'wayland-*' | grep -q ." % runtime
    )
    display = "$(basename $(find %s -maxdepth 1 -type s -name 'wayland-*' | head -1))" % runtime

    with subtest("solar wallpaper obtains a real GeoClue fix"):
        machine.execute(
            "runuser -u alice -- env XDG_RUNTIME_DIR=%s WAYLAND_DISPLAY=%s "
            "genkan wallpaper --file ${fixture} --solar --reduce-motion "
            ">/tmp/solar.log 2>&1 & echo $! >/tmp/solar.pid"
            % (runtime, display)
        )
        machine.wait_until_succeeds(
            "grep -F 'solar schedule applied' /tmp/solar.log",
            timeout=timedelta(seconds=90),
        )
        machine.fail("grep -F 'solar location' /tmp/solar.log")
        machine.succeed("kill $(cat /tmp/solar.pid)")

    with subtest("ordinary h24 wallpaper never requests location"):
        machine.execute(
            "runuser -u alice -- env XDG_RUNTIME_DIR=%s WAYLAND_DISPLAY=%s "
            "genkan wallpaper --file ${fixture} --reduce-motion "
            ">/tmp/static.log 2>&1 & echo $! >/tmp/static.pid"
            % (runtime, display)
        )
        machine.sleep(timedelta(seconds=3))
        machine.succeed("kill -0 $(cat /tmp/static.pid)")
        machine.succeed("kill $(cat /tmp/static.pid)")
        machine.fail("grep -F 'solar' /tmp/static.log")
  '';
}
