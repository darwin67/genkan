{ genkanE2e, genkan }:
{
  name = "genkan-greetd-e2e";

  nodes.machine =
    { config, lib, pkgs, ... }:
    let
      session = pkgs.writeShellScript "genkan-e2e-session" ''
        set -eu
        ${pkgs.coreutils}/bin/env > /tmp/genkan-e2e.env
        ${pkgs.coreutils}/bin/id -un > /tmp/genkan-e2e.user
        printf '%s\0' "$@" > /tmp/genkan-e2e.argv
        ${pkgs.coreutils}/bin/touch /tmp/genkan-e2e.passed
        exec ${pkgs.coreutils}/bin/sleep infinity
      '';
      sessionPackage = (pkgs.writeTextDir "share/wayland-sessions/genkan-e2e.desktop" ''
        [Desktop Entry]
        Type=Application
        Name=Genkan E2E
        Exec=${session}
      '').overrideAttrs {
        passthru.providedSessions = [ "genkan-e2e" ];
      };
    in
    {
      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        password = "correct-password";
      };

      services.greetd = {
        enable = true;
        settings.default_session.command = lib.concatStringsSep " " [
          "${genkanE2e}/bin/genkan-greetd-e2e"
          "--username alice"
          "--wrong-password wrong-password"
          "--password correct-password"
          "--session-command ${session}"
        ];
      };
      services.displayManager.sessionPackages = [ sessionPackage ];
      systemd.services.greetd.environment.XDG_DATA_DIRS =
        "${config.services.displayManager.sessionData.desktops}/share";
    };

  # A separate real greeter preserves the protocol driver's assertions above.
  # Headless Sway provides deterministic screencopy without a physical GPU.
  nodes.pictures =
    { config, pkgs, ... }:
    let
      compositor = pkgs.writeText "genkan-avatar-sway.conf" ''
        output * mode 1280x960 scale 1
        seat seat0 fallback true
        for_window [app_id="genkan"] fullscreen enable
        exec ${genkan}/bin/genkan login --reduce-motion
      '';
      session = (pkgs.writeTextDir "share/wayland-sessions/avatar-test.desktop" ''
        [Desktop Entry]
        Type=Application
        Name=Avatar test
        Exec=${pkgs.coreutils}/bin/sleep infinity
      '').overrideAttrs { passthru.providedSessions = [ "avatar-test" ]; };
    in
    {
      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        password = "unused-test-password";
      };
      services.accounts-daemon.enable = true;
      security.polkit.enable = true;
      services.greetd = {
        enable = true;
        settings.default_session.command = "${pkgs.sway}/bin/sway --config ${compositor}";
      };
      services.displayManager.sessionPackages = [ session ];
      systemd.services.greetd.environment = {
        XDG_DATA_DIRS = "${config.services.displayManager.sessionData.desktops}/share";
        WLR_BACKENDS = "headless";
        WLR_HEADLESS_OUTPUTS = "1";
        WLR_RENDERER = "pixman";
        ICED_BACKEND = "tiny-skia";
      };
      environment.systemPackages = [ genkan pkgs.grim pkgs.sway pkgs.jq pkgs.imagemagick pkgs.tesseract ];
      virtualisation.memorySize = 2048;
      virtualisation.cores = 2;
      system.stateVersion = "26.05";
    };

  testScript = ''
    from datetime import timedelta

    machine.start()
    machine.wait_for_unit("greetd.service")
    xdg_data_dirs = machine.succeed(
        "systemctl show greetd --property=Environment --value"
    ).split("XDG_DATA_DIRS=", 1)[1].split()[0]
    machine.succeed(
        f"test -f {xdg_data_dirs}/wayland-sessions/genkan-e2e.desktop"
    )
    machine.wait_until_succeeds(
        "test -f /tmp/genkan-e2e.passed",
        timeout=timedelta(seconds=60),
    )
    machine.succeed("grep -Fx 'GENKAN_E2E=passed' /tmp/genkan-e2e.env")
    machine.succeed("grep -Fx 'alice' /tmp/genkan-e2e.user")
    import base64
    arguments = base64.b64decode(machine.succeed("base64 -w0 /tmp/genkan-e2e.argv"))
    assert arguments.split(b"\0") == [
        b"space value", b"single'quote", b"$HOME",
        b"$(touch /tmp/genkan-e2e-injected)", b"", b"",
    ], arguments
    machine.fail("test -e /tmp/genkan-e2e-injected")

    pictures.start()
    pictures.wait_for_unit("accounts-daemon.service")
    pictures.succeed("busctl call org.freedesktop.Accounts /org/freedesktop/Accounts org.freedesktop.Accounts FindUserById x 1000")
    uid = pictures.succeed("id -u greeter").strip()
    runtime = f"/run/user/{uid}"

    def greeter(command):
        return (
            f"runuser -u greeter -- env XDG_RUNTIME_DIR={runtime} "
            f"WAYLAND_DISPLAY=$(basename $(find {runtime} -maxdepth 1 -type s -name 'wayland-*' | head -1)) "
            f"SWAYSOCK=$(find {runtime} -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -1) {command}"
        )

    def capture(name, *, differs_from=None):
        pictures.wait_until_succeeds(greeter("swaymsg -t get_tree | jq -e '.. | objects | select(.app_id? == \"genkan\")'"))
        # Wait for the account, not the initial "Select a user" frame. Sway's
        # fullscreen rule fixes the avatar crop and excludes the moving clock.
        pictures.wait_until_succeeds(
            greeter(f"grim /tmp/{name}.png")
            + f" && magick /tmp/{name}.png -crop 300x35+490+778 +repage -resize 300% /tmp/{name}-name.png"
            + f" && tesseract /tmp/{name}-name.png stdout --psm 7 2>/dev/null | grep -iw alice"
        )
        # Account pictures load asynchronously with a two-second bound.
        pictures.sleep(timedelta(seconds=4))
        screenshot = (
            greeter(f"grim /tmp/{name}.png")
            + f" && magick /tmp/{name}.png -crop 30x30+625+720 +repage /tmp/{name}-avatar.png"
        )
        if differs_from is not None:
            # Wait for presentation too: software rendering may lag the
            # account label even after its picture has finished decoding.
            pictures.wait_until_succeeds(
                screenshot
                + f" && test $(magick /tmp/{differs_from}-avatar.png /tmp/{name}-avatar.png"
                + " -compose difference -composite -format '%[fx:mean>0.1]' info:) = 1"
            )
        else:
            pictures.succeed(screenshot)
        pictures.copy_from_machine(f"/tmp/{name}.png")

    def difference(left, right):
        return float(pictures.succeed(
            f"magick /tmp/{left}-avatar.png /tmp/{right}-avatar.png "
            "-compose difference -composite -format '%[fx:mean]' info:"
        ).strip())

    pictures.succeed("runuser -u alice -- genkan avatar clear")
    pictures.succeed("systemctl restart greetd")
    capture("initials")
    # The control must be a rendered avatar, not a blank compositor frame.
    assert int(pictures.succeed("magick /tmp/initials-avatar.png -format '%k' info:")) > 16
    pictures.succeed("runuser -u alice -- genkan avatar set lake")
    pictures.succeed("cmp /var/lib/AccountsService/icons/alice ${genkan}/share/genkan/avatars/lake.png")
    pictures.succeed("systemctl restart greetd")
    capture("builtin", differs_from="initials")
    assert difference("initials", "builtin") > 0.1, "built-in picture did not replace initials"

    # Exercise the real photo-import command, including the private temporary
    # file remaining readable until AccountsService copies it.
    pictures.succeed("runuser -u alice -- genkan avatar set --file ${genkan}/share/genkan/avatars/moon.png")
    assert pictures.succeed("magick identify -format '%m %wx%h' /var/lib/AccountsService/icons/alice").strip() == "PNG 512x512"
    pictures.fail("find /tmp -maxdepth 1 -name 'genkan-avatar-*.png' | grep .")

    pictures.succeed("printf 'not an image' >/tmp/malformed.png; chmod 644 /tmp/malformed.png")
    pictures.succeed(
        "runuser -u alice -- busctl call org.freedesktop.Accounts "
        "/org/freedesktop/Accounts/User1000 org.freedesktop.Accounts.User SetIconFile s /tmp/malformed.png"
    )
    pictures.succeed("cmp /tmp/malformed.png /var/lib/AccountsService/icons/alice")
    pictures.succeed("systemctl restart greetd")
    capture("malformed-initials")
    assert difference("initials", "malformed-initials") < 0.005, "malformed picture did not fall back to initials"
    pid = pictures.succeed(greeter("swaymsg -t get_tree | jq -r '.. | objects | select(.app_id? == \"genkan\") | .pid'")).strip()
    pictures.sleep(timedelta(seconds=3))
    pictures.succeed(f"kill -0 {pid}")
    pictures.succeed("systemctl is-active greetd")
    pictures.succeed("runuser -u alice -- genkan avatar clear")
  '';
}
