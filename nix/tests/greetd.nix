{ genkanE2e }:
{
  name = "genkan-greetd-e2e";

  nodes.machine =
    { config, lib, pkgs, ... }:
    let
      session = pkgs.writeShellScript "genkan-e2e-session" ''
        set -eu
        ${pkgs.coreutils}/bin/env > /tmp/genkan-e2e.env
        ${pkgs.coreutils}/bin/id -un > /tmp/genkan-e2e.user
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
  '';
}
